# Windows NTFS ACL support (#2306)

This document specifies how oc-rsync mirrors discretionary and system
access control entries on NTFS volumes when `-A`/`--acls` is requested,
how those entries cross the wire, and how they translate to and from
POSIX ACLs. It supersedes ad-hoc notes and pins the implementation plan
that issues WAS-2 through WAS-8 will execute.

No wire-protocol changes. No deviation from upstream rsync's ACL byte
stream. NTFS specifics are confined to the `metadata` crate.

## 1. Current state

- Upstream rsync 3.4.1 has no Windows-specific code in `acls.c`. The
  cygwin port (`__CYGWIN__` in `syscall.c`, `util1.c`, `rsync.c`) relies
  on cygwin's POSIX emulation layer; the file's DACL is exposed as a
  synthesised POSIX ACL through cygwin's libacl shim. oc-rsync has no
  cygwin dependency and runs as a native MSVC binary.
- `crates/metadata/src/acl_windows.rs` implements a Tier 1C beta path:
  DACL read via `GetNamedSecurityInfoW`, DACL write via
  `SetNamedSecurityInfoW`, allow-ACE only, SID to RID synthesis, SACL
  skipped. The crate already depends on the safe `windows` crate
  (`Win32_Security`, `Win32_Security_Authorization`,
  `Win32_Storage_FileSystem`, `Win32_System_SystemServices`).
- Public API is identical across all platforms:
  `get_rsync_acl(path, mode, is_default)`, `apply_acls_from_cache(...)`,
  `sync_acls(src, dst, follow_symlinks)`, `default_perms_for_dir(...)`.
  These are re-exported from `crates/metadata/src/lib.rs` and consumed
  unchanged by `crates/transfer` and `crates/protocol/src/acl`.
- Gaps relative to a complete Windows ACL story: deny ACEs are dropped;
  audit ACEs (SACL) are not read or written; inherited ACE flags are
  collapsed; protected DACL bits are not round-tripped; owner and group
  SIDs are not transmitted; hardlink double-application is not guarded.

## 2. NTFS ACL model

An NTFS security descriptor carries five distinct elements:

| Element | Purpose | Wire-mappable? |
|---|---|---|
| Owner SID | Account that owns the object | Lossy via name string |
| Group SID | Primary group (rarely populated outside POSIX-on-Windows) | Lossy via name string |
| DACL | Ordered list of allow and deny ACEs that drive access checks | Partial - allow only |
| SACL | Ordered list of audit and mandatory-integrity ACEs | Skipped; requires `SE_SECURITY_NAME` |
| Control flags | Inheritance, protected, auto-inherited, defaulted bits | Partial - inheritance dropped |

A DACL contains zero or more ACEs. Each ACE has:

- `AceType` byte: `ACCESS_ALLOWED_ACE_TYPE`, `ACCESS_DENIED_ACE_TYPE`,
  audit, alarm, object-allowed, callback variants, etc.
- `AceFlags` byte: inheritance (`OBJECT_INHERIT_ACE`,
  `CONTAINER_INHERIT_ACE`, `NO_PROPAGATE_INHERIT_ACE`,
  `INHERIT_ONLY_ACE`), audit flags (`SUCCESSFUL_ACCESS_ACE_FLAG`,
  `FAILED_ACCESS_ACE_FLAG`), `INHERITED_ACE`.
- `AccessMask`: 32-bit Win32 access mask
  (`FILE_GENERIC_READ`/`WRITE`/`EXECUTE`, `DELETE`, `WRITE_DAC`,
  `WRITE_OWNER`, `SYNCHRONIZE`, ...).
- Trailing SID identifying the trustee.

Order matters. The Windows access-check algorithm walks ACEs in stored
order, so an allow ACE that precedes a deny ACE grants access even if
the same trustee is later denied. `SetNamedSecurityInfoW` re-canonicalises
ACE order when the descriptor is non-protected; protected descriptors
preserve the caller's order verbatim. oc-rsync must therefore decide
whether to mark transmitted DACLs as protected (see section 4).

## 3. SDDL serialisation

The Security Descriptor Definition Language (SDDL) is the textual form
of a security descriptor. A canonical example:

```
O:BAG:SYD:(A;;FA;;;BA)(A;OICI;FR;;;BU)(D;;FW;;;WD)
```

Field breakdown:

- `O:BA` - owner is the built-in Administrators alias.
- `G:SY` - group is the LocalSystem account.
- `D:` - DACL section.
- `(A;;FA;;;BA)` - allow (`A`) ACE, no flags, full access (`FA`),
  trustee Built-in Administrators (`BA`).
- `(A;OICI;FR;;;BU)` - allow, object-inherit and container-inherit,
  generic read (`FR`), Built-in Users (`BU`).
- `(D;;FW;;;WD)` - deny generic write to Everyone (`WD`).

SDDL is what `ConvertSecurityDescriptorToStringSecurityDescriptorW` and
`ConvertStringSecurityDescriptorToSecurityDescriptorW` produce and
consume. It is human-readable, diff-friendly, and the format used by
PowerShell `Get-Acl`/`Set-Acl` output.

oc-rsync uses SDDL only as an opaque wire-format payload for the
Windows-to-Windows fast path (section 4). All cross-platform code paths
operate on the parsed `RsyncAcl` representation already in use by the
POSIX implementation.

## 4. Wire format mapping

Three transport modes coexist behind a single `-A` switch.

### 4.1 Cross-platform mode (always available)

The default. The sender lowers the NTFS DACL to a `RsyncAcl`
(`access_mask_to_rsync_perms` already does this) and emits the standard
upstream wire format from `crates/protocol/src/acl/wire/send.rs`:
varint count, per-entry tag, perm triplet, optional name. The receiver
consumes the same byte stream regardless of platform and either applies
it via `exacl` on POSIX or via `AddAccessAllowedAce` on Windows.

This is the only mode that interoperates with upstream rsync and with
oc-rsync clients on Linux, macOS, FreeBSD.

### 4.2 Windows-to-Windows fidelity mode (opt-in, follow-up)

When both peers advertise the Windows capability bit (negotiated via a
new entry in the capability string, gated on a CLI flag such as
`--windows-acls`), the sender additionally writes the full SDDL string
into the xattr stream under a reserved key. The agreed key is
`user.win32.security_descriptor` (mirrors how Samba stores NT ACLs in
xattrs on Linux backends). The DACL is serialised with
`ConvertSecurityDescriptorToStringSecurityDescriptorW` requesting
`DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION |
GROUP_SECURITY_INFORMATION`. SACL stays out unless the operator passes
`--audit-acls` and the privilege probe succeeds.

The receiver, when both peers negotiated the capability, ignores the
lossy POSIX ACL payload and reconstructs the descriptor from SDDL via
`ConvertStringSecurityDescriptorToSecurityDescriptorW`. Both payloads
are still transmitted so an older receiver falls back gracefully.

This is "either or both": the cross-platform payload is always present,
the SDDL payload is opt-in and additive.

### 4.3 Reject mode

When the operator passes `--fail-on-windows-acl-loss`, the sender
aborts the transfer (exit code 23, partial transfer) if a DACL contains
deny ACEs, audit ACEs, inherited ACEs that cannot be expressed in
POSIX, or owner/group SIDs the receiver cannot resolve. Implemented as
a configuration flag on `RsyncAclLoweringPolicy` checked inside
`get_rsync_acl`.

## 5. Cross-platform translation rules (WAS-4)

### 5.1 DACL to POSIX rwx

Mapping rules, applied in order. The first rule whose trustee matches
wins for the corresponding POSIX bit triplet.

| DACL trustee | POSIX target | Permissions |
|---|---|---|
| File owner SID (resolved via `GetSecurityInfo` owner field) | `user_obj` | rwx mask from access bits |
| Primary group SID | `group_obj` | rwx mask from access bits |
| `Everyone` (`S-1-1-0`) | `other_obj` | rwx mask from access bits |
| Any other user SID | `RsyncAcl::names` entry of kind user | rwx mask + name string |
| Any other group SID | `RsyncAcl::names` entry of kind group | rwx mask + name string |
| Well-known alias (`Authenticated Users`, `BUILTIN\Users`, ...) | `RsyncAcl::names` entry of kind group | rwx mask + name string |

`access_mask_to_rsync_perms` already implements the per-ACE bit mapping:
`FILE_GENERIC_READ` -> `r`, `FILE_GENERIC_WRITE` -> `w`,
`FILE_GENERIC_EXECUTE` -> `x`. Bits outside that triplet
(`DELETE`, `WRITE_DAC`, `WRITE_OWNER`, `SYNCHRONIZE`) are silently
dropped. The reverse mapping (`rsync_perms_to_access_mask`) emits
exactly the three generic bits plus `SYNCHRONIZE` (NTFS requires it for
the descriptor to be usable in opens).

### 5.2 POSIX rwx to DACL

Three ACEs in canonical order:

1. Allow ACE for the owner SID with the mask from `user_obj`.
2. Allow ACE for the primary group SID with the mask from `group_obj`.
   If `mask_obj` is present, intersect with it.
3. Allow ACE for `S-1-1-0` (Everyone) with the mask from `other_obj`.

Additional `RsyncAcl::names` entries become extra allow ACEs after the
three base entries, in the order the wire format presented them. The
DACL is written with `PROTECTED_DACL_SECURITY_INFORMATION` set so
parent inheritance does not silently add ACEs that were never on the
source.

### 5.3 Lossy cases

Documented loss is acceptable; silent loss is not. Each case emits a
one-time warning via the existing `warn_partial_apply` channel:

- Explicit deny ACEs. POSIX has no deny equivalent. The sender drops
  them and warns. Reject mode (4.3) turns the warning into a fatal
  error.
- Inherited ACEs (`AceFlags & INHERITED_ACE`). The sender does not
  transmit them; the receiver relies on the destination's own
  inheritance chain. This matches upstream's treatment of POSIX default
  ACLs as a separate, opt-in stream.
- Audit ACEs (SACL). Not read unless `--audit-acls` is passed and
  `SE_SECURITY_NAME` is held by the sending process. When transmitted,
  carried only in the Windows fidelity payload (4.2); the cross-platform
  payload never includes them.
- Group-specific ACEs that have no POSIX equivalent (e.g. `Authenticated
  Users`, `BUILTIN\Administrators`). Sent as named group entries in the
  cross-platform payload; the POSIX receiver creates a named-group ACL
  entry which `exacl` either applies verbatim (Linux NFSv4 ACLs) or
  drops with a warning (mode-bits-only filesystems).
- Owner and group SIDs without a resolvable account name on the
  receiver. Cross-platform payload encodes the name; if absent, the
  Windows receiver falls back to the synthetic RID and may produce an
  unknown SID. Documented loss; the receiver warns.
- Mandatory integrity labels and `OBJECT_ACE` variants. Skipped
  entirely; not representable in either payload format.

### 5.4 Reverse direction (POSIX source, Windows destination)

`RsyncAcl::from_mode` populates `user_obj`/`group_obj`/`other_obj` from
the file mode. Section 5.2 then constructs the three base ACEs on the
Windows side. Named POSIX entries become extra ACEs by name lookup via
`LookupAccountNameW`; failure to resolve drops the entry with a warning.

## 6. Hardlink handling

NTFS stores one security descriptor per file index. All hardlinks to a
file share that descriptor; writing the DACL through one link mutates
the security descriptor visible via every other link. oc-rsync's
hardlink scheduler (`crates/engine/src/hardlink.rs`) tracks groups of
links pointing at one source file. The Windows ACL path must therefore:

1. Apply DACL writes only to the first link of each hardlink group.
   The scheduler already exposes a "primary link" predicate; ACL
   application piggybacks on the existing primary-link branch in
   `apply_metadata_from_file_entry`.
2. Skip subsequent links rather than no-op-write. A no-op
   `SetNamedSecurityInfoW` still bumps `LastWriteTime` on some NTFS
   configurations and would force unnecessary re-transfers on the next
   run.
3. Validate via `GetFileInformationByHandle` that the destination's
   `nFileIndexHigh`/`nFileIndexLow` matches the expected hardlink group
   before skipping. A mismatch (caused by `--copy-links`, `-L`, or a
   prior transfer breaking the hardlink) reverts to per-file write.

This guard is symmetric with the POSIX path's "apply metadata once per
inode" invariant in `apply_metadata_from_file_entry`; no new public API
is required.

## 7. Crate-of-choice recommendation

Three candidates considered:

- `windows-acl` 0.3 - third-party wrapper around the same `windows-sys`
  surface. Last published 2021. Wraps `GetSecurityInfo`/`SetSecurityInfo`
  in `SecurityDescriptor` and `ACL` types. Unmaintained; pulls
  `winapi` 0.3 which conflicts with our `windows` 0.62 dependency.
  Rejected.
- `windows-permissions` 0.2 - safer wrapper with typed `SecurityDescriptor`,
  `Sid`, `Ace` types. Maintained but pulls `windows-sys` 0.45, also a
  version-skew problem with the `windows` 0.62 crate we already depend
  on. Adds a second binding generation to the build. Rejected.
- Microsoft `windows` 0.62 - the official, supported binding. Already a
  direct dependency of `metadata` (used by `copy_as.rs`,
  `xattr_windows.rs`, and the existing `acl_windows.rs`). Surfaces are
  `unsafe` but encapsulated; the file in question already has
  `#[allow(unsafe_code)]` per the unsafe-code policy. **Selected.**

The `windows` 0.62 crate is the only choice that keeps the build to a
single Windows binding crate and matches the project's stated
preference for the Microsoft-supported wrapper (see CI matrix doc and
unsafe-code policy in the project guide). Wrapping it in safe local
helpers (`OwnedSecurityDescriptor`, `to_wide`, `sid_to_id_access`,
`lookup_sid` already present) keeps the unsafe surface narrow.

The Authorization features required are already pinned:
`Win32_Security`, `Win32_Security_Authorization`,
`Win32_Storage_FileSystem`, `Win32_System_SystemServices`. SDDL
serialisation in section 4.2 needs the
`ConvertSecurityDescriptorToStringSecurityDescriptorW` symbol which is
in the same `Win32_Security_Authorization` feature; no new features
needed.

## 8. Implementation plan (WAS-2 through WAS-8)

Five steps, each one PR-sized, each scoped to a tracked issue.

1. **WAS-2 - SID mapping and owner/group propagation.** Extend
   `read_dacl` to fetch `OWNER_SECURITY_INFORMATION |
   GROUP_SECURITY_INFORMATION` in addition to DACL. Populate
   `RsyncAcl::owner_uid`/`owner_gid` placeholders so the receiver can
   resolve names on its side. Add `lookup_sid_for_uid` helper for the
   reverse path. Tests: round-trip owner and group names on a
   `windows-latest` runner.
2. **WAS-3 - Deny ACE and inherited ACE diagnostics.** Add an
   `RsyncAclLoweringPolicy` enum (`LossyAccept`, `LossyWarn`, `Reject`).
   Surface counts in `transfer_stats` so `--stats` shows how many ACEs
   were lossy. Implements section 5.3's warning path. Add CLI flag
   `--fail-on-windows-acl-loss`.
3. **WAS-4 - Cross-platform translation matrix tests.** Property-test
   the section 5.1 and 5.2 mappings in
   `crates/metadata/tests/acl_handling.rs`. Add golden vectors:
   POSIX -> SDDL -> POSIX must be lossless for the rwx triplet plus
   one named user and one named group. Document each lossy case with
   an explicit test asserting the warning fires once.
4. **WAS-5 - Hardlink-safe DACL application.** Wire the section 6
   primary-link check into `apply_metadata_from_file_entry`. Add a
   regression test creating a three-link hardlink group with a
   non-trivial DACL and asserting exactly one `SetNamedSecurityInfoW`
   call (probe via a thin trait-based mock).
5. **WAS-6 - SDDL fidelity payload (opt-in).** Add the
   `--windows-acls` flag, the `W` capability character, and the
   `user.win32.security_descriptor` xattr round-trip. Negotiation
   gated on both peers advertising `W`. Falls back silently when one
   peer is upstream rsync or non-Windows oc-rsync.

WAS-7 (SACL audit ACE support) and WAS-8 (Windows-to-POSIX bulk fidelity
mapping rules for non-rwx access bits like `DELETE` and `WRITE_DAC`) are
follow-ups against the same architecture and do not require revisiting
the wire layer.

## 9. Non-goals

- No upstream wire-protocol extension. The Windows fidelity payload
  rides on the existing xattr stream (protocol 30+).
- No changes to the POSIX ACL implementation. `acl_exacl.rs` and the
  upstream behaviour it mirrors are untouched.
- No daemon-only or transport-only changes. All work is in
  `crates/metadata/src/acl_windows.rs` plus tests under
  `crates/metadata/tests/` and CI matrix updates under
  `.github/workflows/`.
- No FFI to the deprecated `windows-acl` or `windows-permissions`
  crates.

## 10. References

- `crates/metadata/src/acl_windows.rs` - current Tier 1C beta
  implementation.
- `crates/metadata/src/acl_exacl.rs` - POSIX baseline, the contract
  the Windows path must satisfy.
- `crates/protocol/src/acl/wire/{send,recv}.rs` - wire format the
  sender and receiver share across platforms.
- `crates/engine/src/hardlink.rs` - hardlink scheduler that gates ACL
  application per inode.
- `docs/design/windows-acl-xattr-ci-matrix.md` (#1869) - CI job
  exercising the Windows ACL and xattr surfaces on `windows-latest`.
- `target/interop/upstream-src/rsync-3.4.1/acls.c` - upstream POSIX
  ACL flow (`get_rsync_acl`, `send_rsync_acl`, `recv_rsync_acl`,
  `set_acl`).
- Microsoft Learn: `GetNamedSecurityInfoW`, `SetNamedSecurityInfoW`,
  `ConvertSecurityDescriptorToStringSecurityDescriptorW`, SDDL grammar.
- Issue refs: #2306 (this doc), WAS-2 through WAS-8.
