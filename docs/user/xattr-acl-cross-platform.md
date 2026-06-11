# Extended attributes and ACLs across platforms

User-facing reference for cross-platform xattr and ACL behaviour when
transferring files with `oc-rsync -A` (ACLs) and `-X` (extended attributes)
between Linux, macOS, and Windows hosts. Covers what survives each direction
pair, what is lost, and how to work around the gaps.

## Overview

### Extended attributes (xattrs)

Extended attributes are arbitrary key-value pairs attached to files and
directories outside the regular data stream. Each platform exposes a different
xattr surface:

- **Linux** partitions xattrs into namespaces (`user.*`, `system.*`,
  `trusted.*`, `security.*`). Unprivileged users can only read and write
  `user.*` attributes.
- **macOS** uses a single flat namespace. Apple reserves the `com.apple.*`
  prefix for system-managed metadata - resource forks, Finder info,
  quarantine flags, Spotlight tags, and download provenance.
- **Windows** has no POSIX xattr API. Instead, NTFS provides Alternate Data
  Streams (ADS) - named byte streams attached to files alongside the primary
  unnamed stream. oc-rsync maps each xattr to an ADS when the destination is
  Windows.

When `-X` is passed, oc-rsync enumerates xattrs on the source, sends them
over the wire, and applies them on the destination using the platform's native
storage mechanism. Without `-X`, xattrs are ignored entirely.

### Access control lists (ACLs)

ACLs extend the basic owner/group/other permission model with per-principal
entries. The three platforms use incompatible ACL models:

- **Linux** uses POSIX.1e ACLs - access ACLs on files and directories, plus
  default ACLs on directories that propagate to newly created children.
- **macOS** uses NFSv4-style extended ACLs with a richer permission model
  (14-bit masks, deny entries, inheritance flags). macOS has no concept of
  POSIX default ACLs.
- **Windows** uses NTFS Discretionary ACLs (DACLs) based on Security
  Identifiers (SIDs). DACLs support allow and deny entries, inherited and
  explicit ACEs, and fine-grained permission masks.

When `-A` is passed, oc-rsync reads ACLs from the source, encodes them on the
wire, and applies them on the destination. Cross-platform transfers involve
lossy conversions because the ACL models are structurally different.

### Why cross-platform behaviour matters

rsync is commonly used to move data between heterogeneous hosts - backing up
a macOS workstation to a Linux NAS, syncing a Windows file server to a Linux
replica, or migrating data between cloud instances running different operating
systems. Understanding what metadata survives each platform hop prevents
unexpected permission changes, lost security descriptors, and silent data loss
in auxiliary streams.

---

## Direction matrix

The table below summarises the round-trip fidelity for each source-destination
platform pair. "Full" means byte-for-byte preservation. "Lossy" means some
metadata is converted or dropped. Specific conversions are detailed in the
per-platform sections below.

### ACL round-trip

| Source \ Dest | Linux | macOS | Windows |
|---------------|-------|-------|---------|
| **Linux** | Full POSIX.1e (access + default) | Lossy - default ACLs dropped; access ACLs converted to extended ACLs | Lossy - collapsed to three allow ACEs via POSIX-to-DACL mapping |
| **macOS** | Lossy - deny/audit/alarm ACEs dropped; only rwx-representable entries survive | Full extended ACLs (inheritance flags, granular bits, ordering preserved) | Lossy - same POSIX-to-DACL collapse as Linux to Windows |
| **Windows** | Lossy - DACL collapsed to rwxrwxrwx mode bits; deny/inherited ACEs dropped | Lossy - same DACL-to-POSIX collapse; destination does not synthesize extended ACEs | Full DACL round-trip (owner, group, allow/deny ACEs preserved; SACL excluded) |

### Xattr round-trip

| Source \ Dest | Linux | macOS | Windows |
|---------------|-------|-------|---------|
| **Linux** | Full for `user.*`; `trusted.*`/`security.*` require root on both sides; `system.*` always skipped | `user.*` round-trips (prefix stripped on macOS receiver) | Xattrs stored as NTFS Alternate Data Streams |
| **macOS** | `com.apple.*` stored as `user.com.apple.*`; bytes preserved | Full - all xattrs including `com.apple.*` and resource forks | Xattrs stored as ADS; colon-bearing names may be mangled (see limitations) |
| **Windows** | ADS stored as xattrs; names without `user.*` prefix require root receiver | ADS stored as macOS xattrs verbatim | Full ADS round-trip (unnamed primary stream excluded) |

---

## Per-platform behaviour

### Linux

**ACLs.** Linux uses POSIX.1e ACLs managed through `setfacl`/`getfacl`. Both
access ACLs and directory default ACLs are supported. oc-rsync uses the `exacl`
crate to read and apply ACLs. ID mapping honours `--usermap`, `--groupmap`, and
`--numeric-ids`.

**Xattrs.** The Linux kernel partitions xattrs into four namespaces:

| Namespace | Read/write | Notes |
|-----------|------------|-------|
| `user.*` | Any user | Primary namespace for application data |
| `trusted.*` | Root only | Survives transfer only when both sender and receiver run as root |
| `security.*` | Root only | Same privilege requirement as `trusted.*` |
| `system.*` | Always skipped | oc-rsync and upstream rsync never transfer `system.*` attributes |

Privilege asymmetry (root sender, non-root receiver) causes `trusted.*` and
`security.*` attributes to be silently dropped on the receiver. This matches
upstream rsync behaviour.

**NFSv4 ACLs.** When both endpoints have NFSv4 semantics, the `system.nfs4_acl`
extended attribute passes through the xattr pipeline. This requires root on
both sides since it lives in the `system.*` namespace.

**SELinux contexts.** On RHEL, Fedora, CentOS and other SELinux-enforcing
distributions, file labels live in `security.selinux` and are stored as
opaque bytes by the kernel LSM hook. Under `--xattrs`, oc-rsync preserves
these labels byte-for-byte, matching upstream rsync's `xattrs.c` handling
(`rsync_xal_get()` lines 254-258, `receive_xattr()` lines 828-839). The
namespace passes through the wire verbatim; no SELinux library linkage
is required at either end.

Operational requirements:

- The **sender** must be able to read `security.selinux`. On Linux this
  is unrestricted for root and is also permitted for the file owner on
  most policies; non-root readers without the necessary policy grant
  will not see the namespace and the label is silently omitted from the
  transfer (matching upstream).
- The **receiver** must have `CAP_SYS_ADMIN` (typically root) OR an
  SELinux policy grant that allows writing to `security.selinux`.
  Without one of these the kernel returns `EPERM` for the `setxattr`
  call and the file lands with the receiving host's default context
  (e.g. `unconfined_u:object_r:default_t:s0`). For SELinux-protected
  services (`httpd`, `postgresql`, `sshd`, ...) the wrong context will
  cause AVC denials in enforcing mode; either run the receiver as root,
  re-label after transfer (`restorecon -R`), or stage the destination
  on a non-enforcing host.
- Copying to a destination **without SELinux loaded** is still safe:
  oc-rsync writes the raw bytes, the kernel either stores them (LSM
  supports the namespace) or returns `EOPNOTSUPP`. When the destination
  filesystem accepts the write, a later restore to an enforcing host
  reconstructs the original label.

### macOS

**ACLs.** macOS uses NFSv4-style extended ACLs with a richer model than
POSIX.1e. Features that have no POSIX equivalent include deny entries, granular
14-bit permission masks, audit and alarm ACE types, and inheritance flags.
oc-rsync uses the `exacl` crate for both macOS and Linux ACLs, handling the
model translation at the library level.

macOS has no concept of directory default ACLs. When receiving from a Linux
source, default ACLs are dropped - there is no destination-side representation.

**Xattrs.** macOS uses a flat xattr namespace with no `user.*`/`system.*`
partition. All names are accepted. The `com.apple.*` prefix is reserved by Apple
for system-managed metadata:

| Attribute | Purpose | Transfer behaviour |
|-----------|---------|-------------------|
| `com.apple.ResourceFork` | Legacy resource fork data | Round-trips as an opaque blob. Payloads above 64 MiB are truncated - see limitations below. |
| `com.apple.FinderInfo` | 32-byte Finder type/creator codes and flags | Round-trips byte-for-byte. The macOS kernel enforces the 32-byte size constraint. |
| `com.apple.quarantine` | Gatekeeper download provenance flag | Survives transfer. A backup-restored file is re-quarantined on next launch. |
| `com.apple.metadata:_kMDItemUserTags` | Finder colour tags (bplist) | Round-trips on POSIX destinations. Mangled on Windows due to the colon in the name. |
| `com.apple.metadata:kMDItemWhereFroms` | Download origin URL (bplist) | Same as above. |

**Wire format.** The macOS sender prepends `user.` to every xattr name before
sending it on the wire. The macOS receiver strips this prefix on arrival. This
matches upstream rsync exactly. When transferring to Linux, the name arrives as
`user.com.apple.ResourceFork` and is stored verbatim - Linux consumers cannot
interpret the payload, but a subsequent Linux-to-macOS transfer restores it.

**HFS+ vs APFS.** Both filesystems support xattrs. APFS has no practical
difference from HFS+ for xattr round-trip behaviour. ExFAT and FAT32 volumes
do not support xattrs; writes to those volumes produce per-attribute I/O
errors.

### Windows

**ACLs.** Windows uses NTFS Discretionary ACLs (DACLs) based on Security
Identifiers (SIDs). oc-rsync reads DACLs via `GetNamedSecurityInfoW` and
applies them via `SetNamedSecurityInfoW`. System ACLs (SACLs) are deliberately
excluded - they require the `SE_SECURITY_NAME` privilege and are not part of
the rsync protocol.

When receiving from a POSIX source (Linux or macOS), oc-rsync synthesizes three
canonical allow ACEs from the rwxrwxrwx mode bits:

- Owner SID receives the owner permission triplet.
- Group SID receives the group permission triplet.
- Everyone SID receives the other permission triplet.

POSIX named-user and named-group ACEs survive only if the principal resolves to
a Windows account via `LookupAccountNameW`. Unresolvable principals are
dropped.

When sending to a POSIX destination, the DACL is collapsed to rwxrwxrwx mode
bits via `dacl_to_posix_mode`. Deny ACEs, inherited ACEs, and permission bits
outside the file-read/file-write/file-execute set are dropped with a one-shot
warning.

**`--usermap`/`--groupmap`/`--chown`** are unavailable on Windows and return an
error if specified.

**Xattrs (Alternate Data Streams).** NTFS allows files to carry multiple named
data streams. oc-rsync maps each xattr to an ADS via the `path:name:$DATA`
convention. The unnamed primary stream (`::$DATA`) - which is the file's
regular content - is always excluded.

Without `-X`, ADS are silently dropped during transfer, matching upstream rsync
behaviour on Cygwin. A one-shot warning is emitted when ADS are detected on the
source but `-X` is not enabled.

Non-NTFS volumes (FAT32, exFAT) do not support ADS. Writes to those volumes
fail with an I/O error.

The ADS round-trip through `--xattrs` is exercised end-to-end (not just by
unit tests over the FFI primitives) in
`crates/metadata/tests/windows_ads_xattrs_roundtrip.rs`. The harness spawns a
real `oc-rsync --xattrs --archive` subprocess against a temp source tree
carrying `Zone.Identifier` and a custom stream, then reads the destination
back through the public `read_xattrs_for_wire` API so the receiver's
`FindFirstStreamW` + `FindNextStreamW` enumeration is part of the assertion.
Tests skip cleanly on non-Windows hosts, builds without the `xattr` feature,
non-NTFS scratch volumes, and runners where the `oc-rsync` binary cannot be
located.

---

## Known limitations and gaps

### Lossy conversions that cannot be avoided

These are structural mismatches between the platform metadata models. No
configuration or flag can recover the lost information:

- **POSIX default ACLs to macOS or Windows.** macOS and Windows have no
  equivalent of POSIX default ACLs. They are dropped on the destination.
- **macOS deny/audit/alarm ACEs to Linux.** POSIX.1e has no deny, audit, or
  alarm ACE types. These entries are dropped.
- **DACL-to-POSIX collapse.** A Windows DACL with fine-grained permission
  masks, deny entries, and inheritance flags collapses to nine permission bits
  (rwxrwxrwx) when sent to Linux or macOS.
- **POSIX-to-DACL collapse.** Named-user and named-group ACEs whose principals
  do not resolve via `LookupAccountNameW` are dropped. The POSIX mask ACE is
  folded into the group entry.
- **SACL exclusion.** System ACLs are never transferred. This is a deliberate
  policy matching the privilege requirements of the upstream protocol.

### Xattr limitations

- **`system.*` namespace always skipped.** Matches upstream rsync policy. The
  `system.*` namespace on Linux is never sent regardless of privilege level.
- **Privilege-gated namespaces.** `trusted.*` and `security.*` are silently
  dropped when the receiver does not run as root.
- **Resource fork truncation above 64 MiB.** The `xattr` crate reads resource
  forks with a single `getxattr(2)` call using `position=0`. macOS caps a
  single call at 64 MiB. Upstream rsync loops with rising `position` arguments
  to read arbitrarily large resource forks. This affects only legacy media
  files with very large resource forks - a rare scenario in practice.
- **Colon-in-xattr-name collision on Windows.** macOS xattr names containing
  colons (such as `com.apple.metadata:_kMDItemUserTags`) collide with the
  Win32 ADS stream-name separator. The ADS path
  `file:com.apple.metadata:_kMDItemUserTags:$DATA` is parsed by Win32 as
  stream `com.apple.metadata` of type `_kMDItemUserTags`, mangling the name.
  This affects macOS-to-Windows transfers of Finder tags and download origin
  metadata.
- **ADS namespace filtering on Linux receiver.** ADS stream names from a
  Windows source land in the `user.*` namespace on Linux only if the name
  already carries the `user.` prefix. Names without it are rejected by the
  namespace policy for non-root receivers.
- **Windows domain SIDs.** SID-only ACEs replay correctly only if the
  destination can resolve the same SID. Domain-bound SIDs may not resolve when
  transferring between workgroup machines.

### Validation status

Not all platform combinations have been exercised by automated tests. The
current validation state:

| Direction | Xattr | ACL |
|-----------|-------|-----|
| Linux to Linux | Validated | Validated |
| macOS to macOS | Partial (gated interop test) | Partial (gated interop test) |
| macOS to Linux | Partial (`com.apple.*` audit) | Untested |
| Linux to macOS | Untested | Untested |
| Any to Windows | Untested (simulated mapping tests only) | Untested (simulated mapping tests only) |
| Windows to Any | Untested (simulated mapping tests only) | Untested (simulated mapping tests only) |
| Windows to Windows | Untested | Untested |

Validated means exercised by in-tree interop tests against upstream rsync
3.4.1/3.4.2 and confirmed byte-equivalent. Partial means exercised by gated
interop tests or audit analysis with some edge cases remaining. Untested means
the spec exists but no automated round-trip test has run.

---

## CLI flags

### `--acls` (`-A`)

Enables ACL preservation. Without this flag, ACLs are not read from the source
and not applied on the destination. The flag must be present on both sender and
receiver for ACL transfer to occur.

```sh
oc-rsync -aA /src/ user@host:/dst/
```

### `--xattrs` (`-X`)

Enables extended attribute preservation. Without this flag, xattrs (and
Windows ADS) are ignored. On Windows, a one-shot warning is emitted when ADS
are detected on the source but `-X` is not enabled.

```sh
oc-rsync -aX /src/ user@host:/dst/
```

### Combined usage

Both flags can be combined. The shorthand `-aAX` is equivalent to
`-rlptgoD --acls --xattrs`:

```sh
oc-rsync -aAX /src/ user@host:/dst/
```

### `--numeric-ids`

Preserves numeric uid/gid on the wire without name resolution. Affects ACL
entries that reference principals by numeric id. Useful for cross-host
transfers where user databases differ.

### `--usermap` / `--groupmap` / `--chown`

Remap uid/gid on the receiver. These flags affect which principals appear in
destination ACLs. Not available on Windows - returns an error if specified.

---

## Recommendations for cross-platform syncs

### Linux to Linux

The simplest case. Use `-aAX` for full fidelity. If both sides run as root,
`trusted.*` and `security.*` xattrs are also preserved. No special
considerations.

```sh
oc-rsync -aAX /src/ root@remote:/dst/
```

### macOS to macOS

Use `-aAX`. All xattrs including `com.apple.*` attributes round-trip
faithfully. Extended ACLs with inheritance flags and deny entries are
preserved. Be aware that `com.apple.quarantine` survives - restored files may
trigger Gatekeeper prompts.

```sh
oc-rsync -aAX /src/ user@mac-dest:/dst/
```

### macOS to Linux (backup staging)

Use `-aAX`. Apple metadata (`com.apple.FinderInfo`, `com.apple.ResourceFork`,
Finder tags) is stored verbatim on Linux under the `user.com.apple.*`
namespace. The bytes are opaque to Linux tools but survive a subsequent
Linux-to-macOS restore. macOS ACLs are converted to POSIX.1e with loss of deny
entries and granular permissions.

```sh
oc-rsync -aAX /Users/me/ root@nas:/backups/mac/
```

To restore back to macOS later:

```sh
oc-rsync -aAX root@nas:/backups/mac/ /Users/me/
```

The `com.apple.*` attributes re-emerge with their original names on the macOS
receiver.

### Linux to Windows

Use `-aAX`. POSIX ACLs are converted to three-ACE DACLs (owner/group/Everyone).
Xattrs are stored as NTFS Alternate Data Streams. Named-user and named-group
ACEs survive only if the principal resolves to a local Windows account. Default
ACLs are lost. NTFS is required on the destination.

### Windows to Linux

Use `-aAX`. The DACL is collapsed to rwxrwxrwx permission bits. ADS are stored
as xattrs in the `user.*` namespace if the names already have that prefix -
otherwise a root receiver is required. The `--numeric-ids` flag is recommended
to avoid SID-to-uid mapping failures.

### macOS to Windows or Windows to macOS

Use `-aAX` with the same caveats as the Linux-to-Windows and Windows-to-Linux
paths respectively. Additionally, macOS xattr names containing colons (Finder
tags, download-origin metadata) are mangled on Windows due to the ADS naming
convention. Content survives but the names may not round-trip cleanly through
a subsequent Windows-to-macOS transfer.

### General tips

- **Always test a representative transfer first.** Run with `-avvAX --dry-run`
  to see what metadata changes will be applied without making any writes.
- **Use `--numeric-ids` for cross-platform transfers.** Name-to-id resolution
  differs across platforms. Numeric ids avoid mismatches.
- **Root on the receiver preserves more metadata.** Privileged namespaces
  (`trusted.*`, `security.*`) and ADS names outside `user.*` require root on
  the Linux receiver.
- **NTFS is required for ADS.** FAT32 and exFAT volumes silently drop ADS.
  Ensure the Windows destination uses NTFS.
- **Quarantine awareness for macOS backups.** Restored files carry the original
  quarantine flag. Remove it with `xattr -d com.apple.quarantine <file>` if
  Gatekeeper prompts are unwanted after a restore.

---

## See also

- [Windows support matrix](windows-support-matrix.md) - full Windows feature
  matrix including ADS, DACL, and permission-bit mapping.
- [Filter rules](filter-rules-status.md) - filter rule support and known gaps.
