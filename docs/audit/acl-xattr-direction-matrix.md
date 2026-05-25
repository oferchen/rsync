# ACL / Xattr Cross-Platform Direction-Matrix Specification (XAP-1)

Status: specification only. No per-cell results yet (those belong to XAP-4
through XAP-8). Tracks the parity gap noted in
`[[project_xattr_acl_cross_platform_parity_gap]]`.

## Scope

This document fixes the matrix of source-platform x destination-platform
combinations that must be exercised for `-A` (ACLs) and `-X` (extended
attributes) round-trips. It pins down, per cell:

- The expected round-trip behaviour (what survives the trip intact).
- Known incompatibilities that the implementation already encodes as lossy
  conversions or hard drops.
- The harness primitive that will exercise the cell. Harness primitives
  themselves are introduced in XAP-2 (ACL harness) and XAP-3 (xattr
  harness); this doc names them so later tasks can wire them up directly.

Out of scope: running the matrix, recording actual results, writing test
fixtures, or changing implementation behaviour.

## Authoritative References

Upstream rsync 3.4.1 is the source of truth for round-trip semantics:

- `target/interop/upstream-src/rsync-3.4.1/acls.c` - send/recv ACL wire
  format, `send_rsync_acl`, `recv_rsync_acl`, `set_acl`, and the lossy
  cross-platform mapping rules at `acls.c:902-928`.
- `target/interop/upstream-src/rsync-3.4.1/xattrs.c` - `rsync_xal_get`,
  `rsync_xal_set`, namespace-permission policy at `xattrs.c:64-68` and
  `xattrs.c:254-257`.

Implementation surface in this repo:

- ACL, POSIX (Linux / macOS / FreeBSD via `exacl`):
  `crates/metadata/src/acl_exacl/mod.rs`,
  `crates/metadata/src/acl_exacl/read.rs`,
  `crates/metadata/src/acl_exacl/apply.rs`,
  `crates/metadata/src/acl_exacl/sync.rs`,
  `crates/metadata/src/acl_exacl/reset.rs`,
  `crates/metadata/src/acl_exacl/default_perms.rs`.
- ACL, NFSv4 (Linux `system.nfs4_acl`, FreeBSD ZFS):
  `crates/metadata/src/nfsv4_acl.rs`,
  `crates/metadata/src/nfsv4_acl_stub.rs`.
- ACL, Windows (NTFS DACL via `GetNamedSecurityInfoW` /
  `SetNamedSecurityInfoW`):
  `crates/metadata/src/acl_windows/mod.rs`,
  `crates/metadata/src/acl_windows/dacl.rs`,
  `crates/metadata/src/acl_windows/posix_map.rs`,
  `crates/metadata/src/acl_windows/sddl.rs`,
  `crates/metadata/src/acl_windows/sync.rs`,
  `crates/metadata/src/acl_windows/xattr.rs`.
- ACL stubs / no-op fallbacks:
  `crates/metadata/src/acl_stub.rs`,
  `crates/metadata/src/acl_noop.rs`.
- ID / name mapping:
  `crates/metadata/src/id_lookup/`,
  `crates/metadata/src/mapping/`,
  `crates/metadata/src/mapping_win.rs` (Windows stub - rejects
  `--usermap`/`--groupmap`/`--chown`).
- Xattr cross-platform shim and namespace policy:
  `crates/metadata/src/xattr.rs`.
- Xattr Unix backend (`xattr` crate wrapper):
  `crates/metadata/src/xattr_unix.rs`.
- Xattr Windows backend (NTFS Alternate Data Streams via
  `FindFirstStreamW` / `path:name:$DATA`):
  `crates/metadata/src/xattr_windows.rs`.
- Xattr stub: `crates/metadata/src/xattr_stub.rs`.

## Harness Primitives (defined by XAP-2 / XAP-3)

The cells below cite primitives that XAP-2 and XAP-3 will produce. They
are named here so cell owners do not invent ad-hoc tooling. Each primitive
is parameterised by `(source_platform, dest_platform, transport)` where
`transport` is one of `local`, `daemon`, `ssh`.

- `acl_roundtrip(set, expect)` - install `set` on the source, run
  `oc-rsync -aAX`, read back from destination, assert against `expect`.
  Implemented by XAP-2.
- `acl_default_roundtrip(set, expect)` - same, but exercises directory
  default ACLs (POSIX only). Implemented by XAP-2.
- `acl_nfsv4_roundtrip(set, expect)` - exercises `system.nfs4_acl`
  passthrough on Linux. Implemented by XAP-2.
- `xattr_roundtrip(set, expect)` - install named xattrs on the source,
  transfer with `-X`, read back. Implemented by XAP-3.
- `xattr_namespace_roundtrip(namespace, set, expect)` - parameterised by
  `user.*`, `system.*`, `trusted.*`, `security.*`, `com.apple.*`, or NTFS
  ADS. Implemented by XAP-3.
- `xattr_privilege_roundtrip(euid, set, expect)` - exercises the root vs
  non-root namespace policy from `xattrs.c:64-68`. Implemented by XAP-3.

## Matrix

Rows are source platform. Columns are destination platform. A `-` in
either ACL or xattr expectations means the underlying primitive
intentionally degrades to a no-op for that direction (matching upstream
behaviour or the documented Windows stub policy).

### Legend

- **POSIX.1e ACL**: access ACL plus directory default ACL. Linux native;
  macOS and FreeBSD expose a superset via `exacl`.
- **NFSv4 ACL**: 14-bit permission model stored in `system.nfs4_acl` on
  Linux, native on FreeBSD ZFS, native on macOS HFS+/APFS as extended
  ACLs.
- **NTFS DACL**: SID-based discretionary ACL. SACLs are deliberately
  skipped (see `crates/metadata/src/acl_windows/mod.rs` module doc,
  "Scope").
- **POSIX-mapped DACL**: lossy bidirectional mapping between rwxrwxrwx
  and three canonical allow ACEs (owner / group / Everyone) per
  `crates/metadata/src/acl_windows/posix_map.rs`.
- **ADS**: NTFS Alternate Data Stream (`path:name:$DATA`), the only
  available xattr surface on Windows.

---

### 1. Linux -> Linux

- **ACL expectation**: full POSIX.1e round-trip of access ACL plus
  directory default ACL. NFSv4 ACLs round-trip via the
  `system.nfs4_acl` xattr passthrough when both endpoints have NFSv4
  semantics. ID mapping honours `--usermap`/`--groupmap`/`--numeric-ids`.
- **Xattr expectation**: full round-trip of `user.*` always; `trusted.*`
  and `security.*` when both sender and receiver run as root;
  `system.*` is always skipped (per `xattrs.c:64-68, 254-257`).
- **Known incompatibilities**: none beyond the well-defined
  privilege-gated namespaces. Privilege asymmetry (root sender, non-root
  receiver) silently drops `trusted.*` / `security.*` on receive.
- **Harness primitives**: `acl_roundtrip`,
  `acl_default_roundtrip`, `acl_nfsv4_roundtrip`,
  `xattr_namespace_roundtrip` for each of {`user`, `trusted`, `system`,
  `security`}, `xattr_privilege_roundtrip`.

### 2. Linux -> macOS

- **ACL expectation**: access ACL converts to macOS extended ACLs via
  the `exacl` abstraction
  (`crates/metadata/src/acl_exacl/sync.rs`). POSIX user/group ACEs are
  preserved; mask is folded into the destination's group entry as
  upstream does. Directory default ACLs are dropped on the destination -
  macOS HFS+/APFS has no default-ACL concept.
- **Xattr expectation**: `user.*` round-trips. The xattr name is sent
  verbatim; on macOS it is stored under the same name without
  translation. `trusted.*` / `security.*` survive only if both sides run
  as root and the macOS filesystem accepts the namespace (HFS+ / APFS
  treat them as opaque).
- **Known incompatibilities**: macOS lacks default ACLs, so default-ACL
  inheritance is lost. macOS ACLs allow inheritance flags that have no
  POSIX.1e equivalent and will not survive a subsequent round trip back
  to Linux. `system.*` is skipped on send per upstream policy.
- **Harness primitives**: `acl_roundtrip`,
  `acl_default_roundtrip` (expect default ACL dropped on dest),
  `xattr_namespace_roundtrip` for `user`.

### 3. Linux -> Windows

- **ACL expectation**: lossy POSIX -> DACL conversion.
  `posix_mode_to_dacl` synthesises three canonical allow ACEs
  (owner / group / Everyone) from the rwxrwxrwx triplet per
  `crates/metadata/src/acl_windows/posix_map.rs`. POSIX named-user and
  named-group ACEs are dropped unless the principal resolves to a
  Windows account via `LookupAccountNameW`. POSIX mask is collapsed
  into the group ACE.
- **Xattr expectation**: every xattr name maps to an NTFS Alternate
  Data Stream `path:name:$DATA`
  (`crates/metadata/src/xattr_windows.rs`). `user.*` survives; other
  namespaces are stored verbatim as stream names but carry no
  Windows-side privilege gating.
- **Known incompatibilities**:
  - POSIX uid/gid have no native Windows equivalent. ACEs with
    unresolvable principals are dropped (matches `acls.c:902-928`
    lossy cross-platform contract).
  - `--usermap`/`--groupmap`/`--chown` are unavailable on Windows by
    construction (`crates/metadata/src/mapping_win.rs`).
  - Default ACLs are not representable - inheritance flags do not
    map onto NTFS `OBJECT_INHERIT_ACE` / `CONTAINER_INHERIT_ACE`
    bits in this direction.
  - SACL is never written; SACL transfer requires
    `SE_SECURITY_NAME`.
- **Harness primitives**: `acl_roundtrip` (expect POSIX-mapped DACL
  on dest), `xattr_namespace_roundtrip` for `user` (expect ADS on
  dest).

### 4. macOS -> Linux

- **ACL expectation**: macOS extended ACLs convert to Linux POSIX.1e
  where each ACE has a POSIX-representable principal and permission
  mask. ACEs that exercise the NFSv4 superset (deny entries, granular
  permission bits beyond rwx, audit/alarm types) are dropped.
- **Xattr expectation**: full round-trip of names that fall in `user.*`.
  `com.apple.*` xattrs (resource forks, Finder info, quarantine)
  transfer as-is and are stored verbatim on Linux. They are not
  namespace-filtered by `xattrs.c:64-68` because the prefix is not
  `system.*`, but Linux filesystems will reject them unless the user
  has CAP_SYS_ADMIN. Resource-fork content is preserved as a byte
  stream but no Linux consumer interprets it.
- **Known incompatibilities**:
  - Deny / audit / alarm ACEs lost.
  - `com.apple.ResourceFork` is treated as an opaque blob; receivers
    that re-export to macOS will see the resource fork intact, but
    intermediate Linux tools see only a `user.com.apple.ResourceFork`
    xattr.
  - macOS has no POSIX default ACLs to ship.
- **Harness primitives**: `acl_roundtrip`,
  `xattr_namespace_roundtrip` for `user` and `com.apple.*`.

### 5. macOS -> macOS

- **ACL expectation**: full round-trip of extended ACLs via `exacl`
  (`crates/metadata/src/acl_exacl/sync.rs`). Inheritance flags,
  granular permission bits, and ACE ordering are preserved.
- **Xattr expectation**: full round-trip of all xattrs including
  `com.apple.*` and resource forks. No namespace filtering applies on
  macOS (`crates/metadata/src/xattr.rs::is_xattr_permitted` on
  non-Linux returns true unconditionally).
- **Known incompatibilities**: none expected. The only failure mode is
  destination volume rejection of a specific xattr (e.g. ExFAT, MS-DOS
  thumb drives), which surfaces as a per-attribute I/O error.
- **Harness primitives**: `acl_roundtrip`,
  `xattr_namespace_roundtrip` for `user` and `com.apple.*`.

### 6. macOS -> Windows

- **ACL expectation**: extended ACLs degrade to a POSIX rwxrwxrwx
  triplet (the mode bits the source file effectively presents) and
  then through `posix_mode_to_dacl` to three canonical allow ACEs.
  Named-user / named-group ACEs whose principal resolves via
  `LookupAccountNameW` survive; everything else is dropped.
- **Xattr expectation**: every xattr maps to an ADS. `com.apple.*`
  names round-trip as stream names but Windows-native tooling does not
  interpret them. Resource forks survive as ADS blobs and will
  re-emerge correctly on a subsequent Windows -> macOS hop only if the
  name is preserved verbatim.
- **Known incompatibilities**:
  - Lossy ACE collapse (same incompatibility as Linux -> Windows).
  - Long ADS stream names that exceed Windows path limits are
    rejected by `CreateFileW`; the harness must surface this.
  - Default-ACL inheritance is not representable.
- **Harness primitives**: `acl_roundtrip`,
  `xattr_namespace_roundtrip` for `user` and `com.apple.*`.

### 7. Windows -> Linux

- **ACL expectation**: NTFS DACL converts to POSIX rwxrwxrwx via
  `dacl_to_posix_mode` (`crates/metadata/src/acl_windows/posix_map.rs`).
  Owner / group / Everyone allow ACEs project onto the three POSIX
  permission triplets. Deny ACEs, inherited ACEs (`ID` flag), and
  permission bits outside `FR`/`FW`/`FX`/`FA` are dropped with a
  one-shot warning. SACLs are not transmitted. Named-user / named-group
  ACEs whose SID resolves via the upstream
  `lookup_id_pair`-style fallback are encoded with the synthetic
  RID-as-uid convention documented in
  `crates/metadata/src/acl_windows/mod.rs` (Sender bullet).
- **Xattr expectation**: NTFS ADS streams enumerate via
  `FindFirstStreamW` and ship under their bare stream names. On the
  Linux receiver they land as `user.<stream>` only if the name has the
  `user.` prefix already; otherwise the namespace-policy check in
  `crates/metadata/src/xattr.rs::is_xattr_permitted` rejects the name
  for non-root receivers.
- **Known incompatibilities**:
  - Deny ACEs lost.
  - Inherited / protected / auto-inherit DACL bits lost.
  - SIDs that fail `LookupAccountSidW` are dropped on the sender, so
    the receiver never sees them.
  - ADS stream names that do not start with `user.` either require a
    root receiver or are silently filtered.
- **Harness primitives**: `acl_roundtrip` (expect POSIX-mode-equivalent
  on dest), `xattr_namespace_roundtrip` for stream-name patterns
  (`user.*`, bare, `system.*`).

### 8. Windows -> macOS

- **ACL expectation**: same lossy DACL -> POSIX-mode collapse as
  Windows -> Linux. The resulting POSIX bits feed macOS's POSIX layer;
  the destination does not synthesise extended ACEs.
- **Xattr expectation**: ADS streams ship as flat names and land as
  macOS xattrs verbatim. `com.apple.*` is never produced from a
  Windows source unless explicitly placed in an ADS by the user.
- **Known incompatibilities**: identical to Windows -> Linux for the
  ACL side. On the xattr side, macOS will accept any stream name but
  will not interpret it as a resource fork unless it is exactly
  `com.apple.ResourceFork`.
- **Harness primitives**: `acl_roundtrip`,
  `xattr_namespace_roundtrip` for stream-name patterns.

### 9. Windows -> Windows

- **ACL expectation**: NTFS DACL round-trips through
  `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW`
  (`crates/metadata/src/acl_windows/dacl.rs`,
  `crates/metadata/src/acl_windows/sync.rs`). Owner, group, and DACL
  are preserved. Allow and deny ACEs preserve type, principal, mask,
  and per-ACE flags. SACL is deliberately not transferred (privilege
  policy stated in `acl_windows/mod.rs`).
- **Xattr expectation**: full round-trip of named ADS streams via
  `FindFirstStreamW` enumeration plus `CREATE_ALWAYS` writes. The
  unnamed primary stream `::$DATA` is skipped because it is the file's
  main content, not an xattr.
- **Known incompatibilities**:
  - SACL not transferred.
  - Protected-DACL bits and explicit inheritance state are partial
    today; see `crates/metadata/src/acl_windows/mod.rs` "Scope" note
    on Tier 1C beta parity follow-on work.
  - SID-only ACEs (no resolvable account name) replay correctly only
    if the destination resolves the same SID. Domain-bound SIDs may
    not resolve when transferring between workgroup machines.
- **Harness primitives**: `acl_roundtrip`,
  `xattr_namespace_roundtrip` for arbitrary ADS names including
  Unicode.

## Coverage Matrix Summary

| Source \\ Dest | Linux                                  | macOS                                       | Windows                                  |
| -------------- | -------------------------------------- | ------------------------------------------- | ---------------------------------------- |
| Linux          | Full POSIX.1e + NFSv4 + namespaced xattr | Access ACL preserved, default dropped; `user.*` xattr | DACL via POSIX collapse; xattr -> ADS    |
| macOS          | Extended ACL -> POSIX.1e (lossy); xattr survives | Full ACL + all xattrs incl. resource fork   | DACL via POSIX collapse; xattr -> ADS    |
| Windows        | DACL -> POSIX mode (lossy); ADS -> xattr (namespace-gated) | DACL -> POSIX mode (lossy); ADS -> xattr | Full DACL + ADS (SACL excluded)          |

## Cells That Need New Harness Work

XAP-2 and XAP-3 must produce primitives that can run in CI on Linux and
macOS today. Windows-side cells will be wired up under the Windows
parity series (see `[[project_windows_real_world_parity_unclear]]`) once
a Windows runner with NTFS, real SIDs, and a non-trivial domain account
is available. Until then, Windows-source and Windows-destination cells
remain spec-only in this document and will be exercised manually under
XAP-4 through XAP-8 with results recorded in follow-on docs.

## Out-of-Scope (for XAP-1)

- Running the matrix.
- Building the harness primitives themselves (XAP-2, XAP-3).
- Recording per-cell pass/fail results (XAP-4 through XAP-8).
- Changing implementation behaviour to close any of the documented gaps.
