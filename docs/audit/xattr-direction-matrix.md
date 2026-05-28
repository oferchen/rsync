# Xattr/ACL Cross-Platform Direction-Matrix Synthesis (XAP-9)

Synthesizes results from completed XAP tasks (XAP-1, XAP-2, XAP-3,
XAP-8) into a single direction-matrix with per-cell validation status.
Tracks the parity gap noted in
`[[project_xattr_acl_cross_platform_parity_gap]]`.

## Quick-Reference Summary

| Source \ Dest  | Linux xattr | Linux ACL  | macOS xattr | macOS ACL  | Windows xattr | Windows ACL |
|----------------|-------------|------------|-------------|------------|---------------|-------------|
| **Linux**      | Validated   | Validated  | Untested    | Untested   | Untested      | Untested    |
| **macOS**      | Partial     | Untested   | Partial     | Partial    | Untested      | Untested    |
| **Windows**    | Untested    | Untested   | Untested    | Untested   | Untested      | Untested    |

Legend:

- **Validated** - exercised by in-tree interop tests against upstream
  rsync 3.4.1/3.4.2 and confirmed byte-equivalent.
- **Partial** - exercised by gated interop tests or audit analysis; some
  edge cases remain untested (e.g., resource forks > 64 MiB, root-only
  namespaces).
- **Untested** - spec exists (XAP-1) but no automated round-trip test
  has run. Pending XAP-4 through XAP-7.
- **N/A** - the combination is architecturally impossible or degrades to
  a documented no-op.

## XAP Task Status

| Task  | Description                            | Status     | Output                                                   |
|-------|----------------------------------------|------------|----------------------------------------------------------|
| XAP-1 | Direction-matrix spec                  | Complete   | `docs/audit/acl-xattr-direction-matrix.md`               |
| XAP-2 | ACL round-trip harness primitive       | Complete   | `tests/integration/acl_roundtrip.rs`                     |
| XAP-3 | Xattr round-trip harness primitive     | Complete   | `tests/integration/xattr_roundtrip.rs`                   |
| XAP-4 | Linux cross-platform round-trip tests  | Pending    | Requires Linux CI runner with ACL/xattr-capable FS       |
| XAP-5 | macOS -> Linux round-trip tests        | Pending    | Requires macOS + Linux endpoints                         |
| XAP-6 | Windows -> Linux round-trip tests      | Pending    | Requires Windows runner with NTFS + real SIDs             |
| XAP-7 | Windows -> macOS round-trip tests      | Pending    | Requires Windows + macOS endpoints                       |
| XAP-8 | macOS xattr handling audit             | Complete   | `docs/audit/macos-xattr-handling.md`                     |
| XAP-9 | Direction-matrix synthesis (this doc)  | Complete   | `docs/audit/xattr-direction-matrix.md`                   |

## Per-Cell Detail

### 1. Linux -> Linux

**Xattr: Validated**

Round-trip exercised by `tests/acl_xattr_roundtrip_linux.rs` (gated by
`OC_RSYNC_METADATA_INTEROP=1`). Tests both directions:
- oc-rsync sender -> upstream receiver
- upstream sender -> oc-rsync receiver

Covers `user.*` namespace. `trusted.*` and `security.*` round-trip when
both sides run as root. `system.*` is always skipped per upstream policy
(`xattrs.c:254-257`). Privilege asymmetry (root sender, non-root
receiver) silently drops `trusted.*` / `security.*` on receive - matches
upstream.

**ACL: Validated**

Same test file exercises POSIX.1e access ACLs and default ACLs via
`setfacl`/`getfacl`. NFSv4 ACLs round-trip via the `system.nfs4_acl`
xattr passthrough when both endpoints support it. ID mapping honours
`--usermap`/`--groupmap`/`--numeric-ids`.

Harness infrastructure in `tests/integration/acl_roundtrip.rs` uses
Linux `setfacl -m` for stamping and `getfacl -p --omit-header` for
normalized readback.

**Known limitations**: none beyond privilege-gated namespaces.

### 2. Linux -> macOS

**Xattr: Untested** (XAP-5)

Spec from XAP-1: `user.*` round-trips. Wire prefix `user.` is stripped
by the macOS receiver, so `user.foo` lands as `foo`. `trusted.*` /
`security.*` survive only if both sides run as root and the macOS
filesystem accepts the namespace. `system.*` is skipped on send.

No automated test exercises this direction today.

**ACL: Untested** (XAP-5)

Spec from XAP-1: access ACL converts to macOS extended ACLs via the
`exacl` abstraction. POSIX user/group ACEs are preserved. Mask is folded
into the group entry. Directory default ACLs are dropped - macOS
HFS+/APFS has no default-ACL concept.

### 3. Linux -> Windows

**Xattr: Untested** (XAP-6)

Spec from XAP-1: every xattr name maps to an NTFS Alternate Data Stream
(`path:name:$DATA`) via `crates/metadata/src/xattr_windows.rs`.
`user.*` survives. Other namespaces are stored verbatim as stream names
but carry no Windows-side privilege gating.

**ACL: Untested** (XAP-6)

Spec from XAP-1: lossy POSIX -> DACL conversion.
`posix_mode_to_dacl` synthesizes three canonical allow ACEs (owner /
group / Everyone) from the rwxrwxrwx triplet. Named-user and
named-group ACEs are dropped unless the principal resolves to a Windows
account via `LookupAccountNameW`. POSIX mask is collapsed into the group
ACE. `--usermap`/`--groupmap`/`--chown` are unavailable on Windows
(`crates/metadata/src/mapping_win.rs`).

Simulated round-trip tested in `tests/acl_windows_to_linux_roundtrip.rs`
using hardcoded SDDL payloads - exercises the mapping rules without a
real Windows host.

### 4. macOS -> Linux

**Xattr: Partial**

XAP-8 audit (`docs/audit/macos-xattr-handling.md`) confirmed:
- `com.apple.*` xattrs transfer as `user.com.apple.*` on the wire
  (prefix added by macOS sender, `prefix.rs:85-97`).
- Linux receiver stores them verbatim under `user.com.apple.*`.
- Bytes round-trip cleanly back to macOS on a subsequent restore.

`tests/acl_xattr_roundtrip_macos.rs` exercises `com.apple.FinderInfo`,
`com.apple.metadata:_kMDItemUserTags`, and `com.apple.ResourceFork`
through both oc-rsync and upstream rsync - but only on macOS CI
(`OC_RSYNC_METADATA_INTEROP=1`). No test stamps these on a Linux
destination and reads them back.

Known gaps:
- Resource forks > 64 MiB are truncated (finding F2 in XAP-8) - the
  `xattr` crate makes a single `getxattr(2)` call with `position=0`,
  while upstream loops with rising `position` arguments.
- `com.apple.quarantine` survives the transfer - Gatekeeper state
  re-attaches on restore (finding F3; matches upstream).

**ACL: Untested** (XAP-5)

Spec from XAP-1: macOS extended ACLs convert to Linux POSIX.1e where
each ACE has a POSIX-representable principal. Deny entries, granular
NFSv4 permission bits beyond rwx, and audit/alarm ACE types are dropped.

### 5. macOS -> macOS

**Xattr: Partial**

`tests/acl_xattr_roundtrip_macos.rs` exercises the full macOS -> macOS
xattr path including `com.apple.FinderInfo` (32-byte fixed-size),
`com.apple.metadata:_kMDItemUserTags` (bplist with colon in name), and
`com.apple.ResourceFork` (opaque blob). Round-trips through both
oc-rsync and upstream rsync in both directions.

XAP-8 audit confirmed no namespace filtering on macOS
(`is_xattr_permitted` returns `true` unconditionally for non-Linux
targets) - matches upstream exactly.

Known gap: resource fork > 64 MiB truncation (F2). No volume-rejection
test for ExFAT/FAT destinations.

**ACL: Partial**

macOS extended ACLs round-trip via `exacl`. Inheritance flags, granular
permission bits, and ACE ordering are preserved. The `acl_roundtrip.rs`
harness uses macOS `chmod +a` / `ls -led` for stamping and readback.
Exercised in the `acl_roundtrip_local.rs` test (gated by
`OC_RSYNC_ACL_ROUNDTRIP=1`).

No upstream interop assertion exists specifically for macOS ACLs; the
macOS interop test (`acl_xattr_roundtrip_macos.rs`) focuses on xattrs,
not ACLs.

### 6. macOS -> Windows

**Xattr: Untested** (XAP-7)

Spec from XAP-1: every xattr maps to an ADS. `com.apple.*` names
round-trip as stream names but Windows tooling does not interpret them.
Resource forks survive as ADS blobs.

XAP-8 finding (section 7.4): names containing `:` (e.g.,
`com.apple.metadata:_kMDItemUserTags`) collide with the Win32 ADS
stream-name separator. `stream_path_wide` builds the path by literal
concatenation, so the receiver opens
`<file>:com.apple.metadata:_kMDItemUserTags:$DATA` which Win32 parses as
stream `com.apple.metadata` of type `_kMDItemUserTags`. The actual write
target name is mangled.

**ACL: Untested** (XAP-7)

Spec from XAP-1: extended ACLs degrade to a POSIX rwxrwxrwx triplet
(the mode bits the source file presents) then through
`posix_mode_to_dacl` to three canonical allow ACEs. Same lossy collapse
as Linux -> Windows.

### 7. Windows -> Linux

**Xattr: Untested** (XAP-6)

Spec from XAP-1: NTFS ADS streams enumerate via `FindFirstStreamW` and
ship under their bare stream names. On the Linux receiver they land as
`user.<stream>` only if the name already has the `user.` prefix;
otherwise the namespace-policy check rejects the name for non-root
receivers.

**ACL: Untested** (XAP-6)

Spec from XAP-1: NTFS DACL converts to POSIX rwxrwxrwx via
`dacl_to_posix_mode`. Deny ACEs, inherited ACEs, and permission bits
outside `FR`/`FW`/`FX`/`FA` are dropped with a one-shot warning. SACLs
are not transmitted.

Simulated test in `tests/acl_windows_to_linux_roundtrip.rs` validates
the DACL -> POSIX mapping contract using hardcoded SDDL payloads.

### 8. Windows -> macOS

**Xattr: Untested** (XAP-7)

Spec from XAP-1: ADS streams ship as flat names and land as macOS
xattrs verbatim. macOS accepts any name but does not interpret it as a
resource fork unless it is exactly `com.apple.ResourceFork`.

**ACL: Untested** (XAP-7)

Same lossy DACL -> POSIX-mode collapse as Windows -> Linux. The
resulting POSIX bits feed the macOS POSIX permission layer; the
destination does not synthesize extended ACEs.

### 9. Windows -> Windows

**Xattr: Untested** (XAP-6)

Spec from XAP-1: full round-trip of named ADS streams via
`FindFirstStreamW` + `CREATE_ALWAYS`. The unnamed primary stream
`::$DATA` is skipped (it is the file's main content, not an xattr).

**ACL: Untested** (XAP-6)

Spec from XAP-1: NTFS DACL round-trips through
`GetNamedSecurityInfoW` / `SetNamedSecurityInfoW`. Owner, group, and
DACL are preserved. Allow and deny ACEs preserve type, principal, mask,
and per-ACE flags. SACL is deliberately not transferred.

`docs/audit/windows-dacl-ace-inheritance.md` (WPC-10) audits
inherited vs explicit ACE fidelity. Protected-DACL bits and explicit
inheritance state are partial. Domain-bound SIDs may not resolve when
transferring between workgroup machines.

## Harness Primitives

Two round-trip harness primitives are available for future XAP-4..7
tests. They are parameterized by platform and can be composed for any
direction pair.

### ACL Harness (XAP-2)

Location: `tests/integration/acl_roundtrip.rs`

Gate env: `OC_RSYNC_ACL_ROUNDTRIP=1`

Provides:
- `AclEntry` - platform-agnostic ACL entry specification (User, Group,
  DefaultUser, DefaultGroup with permission bits).
- `AclTestFixture::try_build(entries)` - creates a temp directory tree,
  stamps ACLs using platform tools (`setfacl` on Linux, `chmod +a` on
  macOS, `icacls` on Windows), and probes tool availability.
- `AclTestFixture::transfer()` - runs `oc-rsync -aA --numeric-ids`.
- `verify_acl_roundtrip(src, dst, entries)` - reads ACLs back using
  platform tools, normalizes and sorts, returns a structured diff
  (`AclRoundtripResult`).

Platform coverage of the harness itself:
- Linux: POSIX ACLs via `setfacl`/`getfacl`.
- macOS: NFSv4-style ACLs via `chmod +a`/`ls -led`.
- Windows: DACL via `icacls /grant`.

### Xattr Harness (XAP-3)

Location: `tests/integration/xattr_roundtrip.rs`

Gate env: `OC_RSYNC_XATTR_ROUNDTRIP=1`

Provides:
- `XattrEntry` - name/value pair with convenience constructors for
  `user.*`, `security.*`, and macOS-style names.
- `FixtureFile` - file or directory entry with associated xattr list.
- `XattrTestFixture::try_build(entries)` - creates temp dirs, stamps
  xattrs via the `xattr` crate, probes filesystem support.
- `XattrTestFixture::transfer()` - runs `oc-rsync -aX --numeric-ids`.
- `verify_xattr_roundtrip(src, dst, entries)` - reads xattrs back,
  sorts, returns structured diff (`XattrRoundtripResult`).

Platform coverage of the harness itself:
- Linux: `user.*` namespace. `security.*` requires root and is tested
  conditionally.
- macOS: arbitrary names (no namespace prefix required).
  `com.apple.*`-style attributes supported.

### Interop Harness

Location: `tests/integration/acl_xattr_interop_harness.rs`

Gate env: `OC_RSYNC_METADATA_INTEROP=1`

Higher-level harness that bounces a directory tree through both oc-rsync
and upstream rsync using `-aAX`. Provides `MetadataRecord` snapshot and
diffing for combined ACL + xattr comparison. Used by:
- `tests/acl_xattr_roundtrip_linux.rs` (Linux -> Linux, both directions)
- `tests/acl_xattr_roundtrip_macos.rs` (macOS -> macOS, both directions)

## macOS-Specific Findings (XAP-8)

The XAP-8 audit (`docs/audit/macos-xattr-handling.md`) produced six
findings relevant to the direction matrix.

### F1 - No macOS-specific namespace filtering

`is_xattr_permitted` returns `true` unconditionally on every non-Linux
target (`crates/metadata/src/xattr.rs:64-67`). Matches upstream exactly
(`xattrs.c:254-258` only filters under `#ifdef HAVE_LINUX_XATTRS`).

### F2 - Resource fork > 64 MiB chunked-read loop not implemented

Upstream `lib/sysxattrs.c:60-80` loops `getxattr(2)` with rising
`position` arguments for resource forks exceeding 64 MiB. The `xattr`
crate hard-codes `position = 0` and makes one call. On a resource fork
larger than 64 MiB the macOS kernel returns the first 64 MiB and
oc-rsync sees a truncated read. Rare in practice - tracked as XAP-11.

### F3 - Quarantine xattr survives transfer

Neither oc-rsync nor upstream filters `com.apple.quarantine`. A
backup-restored file is re-quarantined by Gatekeeper on next launch.
Matches upstream. Recommendation R1 proposes an opt-in
`--macos-strip-quarantine` flag (XAP-11 follow-up).

### F4 - macOS xattr-write errors propagate as opaque `io::Error`

No Apple-specific error mapping. The underlying errno is preserved via
`MetadataError::source()`. Matches upstream's generic xattr error path.

### F5 - Test coverage is gated

`tests/acl_xattr_roundtrip_macos.rs` exercises the production transfer
pipeline against upstream but requires `OC_RSYNC_METADATA_INTEROP=1`.
Not in default CI. Gap to close under XAP-5.

### F6 - Wire prefix handling matches upstream

macOS sender prepends `user.` (`prefix.rs:85-97`, upstream
`xattrs.c:518-530`). macOS receiver strips it (`prefix.rs:163-188`,
upstream `xattrs.c:832-847`). One deviation (F2), one shared footgun
(F3).

## Gaps and Next Steps

### Blocking gaps (require platform hardware)

| Gap | Direction | Blocked on | XAP task |
|-----|-----------|------------|----------|
| Linux -> macOS xattr/ACL | Cross-platform | macOS + Linux endpoints | XAP-5 |
| macOS -> Linux xattr/ACL | Cross-platform | macOS + Linux endpoints | XAP-5 |
| Any -> Windows xattr/ACL | Windows dest | Windows runner with NTFS, real SIDs | XAP-6 |
| Windows -> Any xattr/ACL | Windows source | Windows runner with NTFS, real SIDs | XAP-6 |
| macOS <-> Windows xattr/ACL | Cross-platform | Both macOS and Windows endpoints | XAP-7 |

### Non-blocking gaps (can be addressed without new hardware)

| Gap | Description | Tracked as |
|-----|-------------|------------|
| Resource fork > 64 MiB | `xattr` crate single `getxattr` call truncates | XAP-11 |
| Quarantine strip flag | Opt-in `--macos-strip-quarantine` for backup use case | XAP-11 |
| Colon-in-xattr-name | `com.apple.metadata:*` names mangle on Windows ADS | WPC-1 |
| macOS ACL interop assertion | No upstream-parity test for macOS ACLs specifically | XAP-5 |
| Windows CI metadata tests | `metadata` crate is excluded from Windows CI matrix | #1869 |

## Cross-References

- XAP-1 direction-matrix spec: `docs/audit/acl-xattr-direction-matrix.md`
- XAP-8 macOS xattr audit: `docs/audit/macos-xattr-handling.md`
- WPC-1 ADS handling audit: `docs/audit/windows-ads-handling.md`
- WPC-10 DACL inheritance audit: `docs/audit/windows-dacl-ace-inheritance.md`
- Windows ACL/xattr CI matrix: `docs/audits/windows-acl-xattr-ci-matrix.md`
- ACL non-root parity: `docs/audits/upstream-3.4.2-acl-non-root-parity.md`
- Memory note: `[[project_xattr_acl_cross_platform_parity_gap]]`
