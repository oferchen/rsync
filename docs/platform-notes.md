# Platform Notes

Cross-platform metadata support in oc-rsync. All stubs compile cleanly on
every tier-1 target; no missing `#[cfg]` gates.

## Feature Matrix

| Feature | Linux | macOS | FreeBSD | Windows |
|---------|:-----:|:-----:|:-------:|:-------:|
| POSIX ACLs (`-A`) | yes (`exacl`) | yes (`exacl`) | yes (`exacl`) | NTFS DACL (Tier 1C partial) |
| NFSv4 ACLs | yes (via xattr) | yes (via xattr) | yes (via xattr) | no-op |
| Extended attributes (`-X`) | yes (`xattr` crate) | yes (`xattr` crate) | yes (`xattr` crate) | NTFS Alternate Data Streams |
| uid/gid preservation (`-o`/`-g`) | yes | yes | yes | identity stub |
| Device/FIFO creation (`-D`) | yes | yes | yes | no-op |
| Symlinks (`-l`) | yes | yes | yes | yes |
| Hardlinks (`-H`) | yes | yes | yes | yes |
| Permissions (`-p`) | yes | yes | yes | read-only bit only |
| Sparse files (`-S`) | yes | yes | yes | yes |
| `--fake-super` xattr storage | yes | yes | yes | `Unsupported` error |

## Windows Behavior

### ACLs (`-A`) - Windows ACL behavior

`--acls` is wired on Windows. The implementation lives in
`crates/metadata/src/acl_windows.rs` and rides on the standard upstream
cross-platform ACL wire format with a reserved xattr slot for full NTFS
fidelity. Three transfer directions are documented below.

#### Windows sender, any receiver

When the source is a Windows file and `--acls` is passed, oc-rsync reads
the NTFS DACL via `GetNamedSecurityInfoW` and emits two payloads:

1. **The cross-platform payload** (always present). The DACL is lowered
   to a POSIX-compatible `RsyncAcl` via `access_mask_to_rsync_perms`:
   `FILE_GENERIC_READ` maps to `r`, `FILE_GENERIC_WRITE` to `w`,
   `FILE_GENERIC_EXECUTE` to `x`. The owner-SID ACE becomes `user_obj`,
   the primary-group-SID ACE becomes `group_obj`, the Everyone
   (`S-1-1-0`) ACE becomes `other_obj`, and additional trustees become
   named user or group entries resolved via `LookupAccountSidW`. This
   payload uses the varint count plus per-entry tag, permission triplet,
   and optional name sequence from `crates/protocol/src/acl/wire/send.rs`.
   It is byte-for-byte identical to what upstream rsync writes for a
   POSIX file with the same logical permissions.
2. **The reserved xattr slot** `user.win32.security_descriptor` carries
   the full security descriptor serialised as SDDL via
   `ConvertSecurityDescriptorToStringSecurityDescriptorW` with
   `DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION |
   GROUP_SECURITY_INFORMATION`. The SDDL string preserves deny ACEs,
   non-`rwx` access bits, named trustees, owner and group SIDs, and the
   protected-DACL flag verbatim. SACL contents are not included.

The two payloads are additive. The cross-platform payload is the
authoritative source of truth on the wire; the SDDL slot is supplemental
fidelity for receivers that understand it.

#### Linux, macOS, or FreeBSD receiver from a Windows sender

The reserved slot `user.win32.security_descriptor` is dropped on receive.
oc-rsync's xattr application path discards any xattr key in the
`user.win32.*` namespace because POSIX backends have no native way to
interpret an SDDL string and storing it as an opaque blob would mislead
local tooling. The lower POSIX bits travel via the standard `RsyncAcl`
payload and are applied through `exacl`, which writes a normal POSIX.1e
ACL on Linux ext4 and a synthesised POSIX-style mask on macOS APFS and
FreeBSD UFS/ZFS. The result is the closest POSIX representation of the
original NTFS permission state; deny ACEs, audit ACEs, and inherited
ACEs are not represented.

#### Windows receiver from a Linux, macOS, or FreeBSD sender

When the source is a POSIX file, the sender emits only the cross-platform
payload; there is no SDDL slot to carry. On Windows the receiver
synthesises a DACL from the POSIX `RsyncAcl` via
`rsync_perms_to_access_mask` (see WAS-4, PR #4361):

1. An allow ACE for the owner SID with the mask from `user_obj`.
2. An allow ACE for the primary group SID with the mask from `group_obj`,
   intersected with `mask_obj` when present.
3. An allow ACE for Everyone (`S-1-1-0`) with the mask from `other_obj`.
4. One additional allow ACE per named user or named group entry, in
   wire order, resolved via `LookupAccountNameW`. Names that fail to
   resolve are dropped with a one-time warning.

The synthesised descriptor is written with
`PROTECTED_DACL_SECURITY_INFORMATION` so parent inheritance does not
silently add ACEs that were never on the source. Each emitted ACE
carries `FILE_GENERIC_READ`/`WRITE`/`EXECUTE` plus `SYNCHRONIZE` (NTFS
requires `SYNCHRONIZE` for the descriptor to be usable in subsequent
opens).

This mapping is lossy and the loss is documented. POSIX has no concept
of `DELETE`, `WRITE_DAC`, `WRITE_OWNER`, audit ACEs, or inheritance
flags, so the resulting DACL is functionally equivalent to the POSIX
source but does not match what a hand-authored Windows DACL would
typically contain. For higher fidelity in Windows-to-Windows transfers,
the planned `--windows-acls` flag will negotiate the SDDL slot
end-to-end (see **--windows-acls** in `docs/oc-rsync.1.md`).

#### What is not supported

- **System ACLs (SACL).** Audit ACEs require the `SE_SECURITY_NAME`
  privilege to read and write, and they cannot ride the cross-platform
  payload. The current path covers the DACL only. The planned
  **--audit-acls** flag will opt in to SACL handling and carry SACL
  contents in the SDDL slot.
- **Mandatory integrity labels** and `OBJECT_ACE` variants. Not
  representable in either payload.
- **Inherited ACEs** (`AceFlags & INHERITED_ACE`). Skipped on send; the
  receiver relies on its own inheritance chain. This mirrors how
  upstream rsync handles POSIX default ACLs as a separate, opt-in
  stream.

#### Hardlink interaction

NTFS stores one security descriptor per file index. All hardlinks to a
file share that descriptor, so writing the DACL through one alias
mutates the descriptor visible via every other alias. oc-rsync's
hardlink scheduler applies the DACL exactly once per hardlink cohort,
on the primary link, and skips the followers. A no-op
`SetNamedSecurityInfoW` on a follower would still bump `LastWriteTime`
on some NTFS configurations and would force unnecessary re-transfers
on the next run. The leader write populates the shared inode and every
follower inherits the DACL for free.

See `docs/audits/windows-hardlink-acl-inheritance.md` (#2311) for the
audit trail and the WAS-6 cohort-guard implementation notes.

#### Further reading

- `docs/oc-rsync.1.md` - the **-A**, **--acls** entry documents the full
  Windows behaviour, the planned **--audit-acls**,
  **--fail-on-windows-acl-loss**, and **--windows-acls** flags, and the
  per-loss warning channel.
- `docs/design/windows-ntfs-acl-support.md` (#2306) - the design
  document with the full DACL-to-POSIX mapping matrix, the wire-format
  decision record, and the WAS-2 through WAS-8 roadmap.
- `crates/metadata/src/acl_windows.rs` - the Tier 1C implementation.
- `crates/protocol/src/acl/wire/{send,recv}.rs` - the cross-platform
  wire format shared with upstream rsync and with POSIX peers.

### Extended Attributes (`-X`)

When `-X` is passed on Windows, `xattr_stub::sync_xattrs()` emits a
**one-time warning** and returns `Ok(())`:

> warning: extended attributes are not supported on this platform; skipping xattr preservation

The `xattr` crate dependency is gated with `cfg(unix)` in `Cargo.toml`, so
it is never compiled on Windows.

**Windows equivalent (not implemented):** NTFS Alternate Data Streams (ADS)
serve a similar role to Unix extended attributes. Access is via the
`filename:streamname` path syntax or `BackupRead`/`BackupWrite` APIs.
Upstream rsync does not support NTFS ADS either.

### NFSv4 ACLs

The NFSv4 ACL module (`nfsv4_acl_stub.rs`) provides type definitions
(`Nfs4Ace`, `Nfs4Acl`, `AceType`, `AceFlags`, `AccessMask`) so code that
references these types compiles on all platforms. All operations return
`Ok(None)`, `Ok(())`, or `false`.

### Ownership (`-o`/`-g`)

`ownership_stub.rs` provides identity functions — uid/gid values pass
through unchanged but are not applied. `id_lookup_stub.rs` returns `None`
for all name-to-id and id-to-name lookups.

### `--fake-super`

`fake_super.rs` returns `ErrorKind::Unsupported` on Windows since it
requires xattr support to store metadata in `user.rsync.%stat` attributes.

## Stub Architecture

```
                   ┌─── Linux/macOS/FreeBSD ──→ acl_exacl.rs (real)
-A flag ──→ cfg ──┤─── iOS/tvOS/watchOS ─────→ acl_stub.rs (warning)
                   └─── everything else ──────→ acl_noop.rs (warning)

                   ┌─── Unix + xattr feature ─→ xattr.rs (real)
-X flag ──→ cfg ──┤
                   └─── otherwise ────────────→ xattr_stub.rs (warning)

                   ┌─── Unix + xattr feature ─→ nfsv4_acl.rs (real)
NFSv4 ACL ─→ cfg ─┤
                   └─── otherwise ────────────→ nfsv4_acl_stub.rs (no-op)
```
