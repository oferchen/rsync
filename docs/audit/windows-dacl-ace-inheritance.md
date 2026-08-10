# Windows DACL inherited vs explicit ACE round-trip audit (WPC-10)

Tracking issue: #2912. Parent series: #2869 (WPC umbrella). Sibling
audits already shipped: #2904 / WPC-1 (ADS handling), #4920 / WPC-13
(support matrix). Baseline DACL/SACL/SDDL plumbing landed across the
WAS-1 through WAS-8 series.

## 1. Scope

This audit answers a single question: when oc-rsync transfers a file
or directory whose Windows DACL contains a mix of **inherited** ACEs
(propagated from a parent) and **explicit** ACEs (set directly on the
object), does the destination DACL preserve the inheritance state of
each ACE, or does it flatten everything to explicit?

The audit covers four code surfaces:

1. **Read side** - how the source DACL is extracted from the file
   system and which per-ACE / per-SD flag bits are inspected.
2. **Wire encoding** - what representation crosses the rsync wire and
   whether the inheritance bit survives the round trip.
3. **Write side** - how the destination DACL is reconstructed and
   whether per-ACE inheritance flags and the `SE_DACL_AUTO_INHERITED`
   security-descriptor control bit are restored.
4. **Test coverage** - whether any existing test pins this behaviour
   end-to-end against a parent / child inheritance tree.

WAS-1 through WAS-8 shipped the cross-platform DACL/SACL/SDDL baseline;
WPC-10 specifically audits the inheritance-flag fidelity gap flagged by
the support matrix at `docs/user/windows-support-matrix.md:97-100` and
by the memory note `[[project_windows_real_world_parity_unclear]]`.

## 2. Background

### 2.1 Windows DACL inheritance model

Each Access Control Entry (ACE) in a DACL is described by an
`ACE_HEADER` containing `AceType`, `AceFlags`, and `AceSize`. The
`AceFlags` byte carries the inheritance metadata:

| Flag                         | Value | Meaning                                                    |
| ---------------------------- | ----- | ---------------------------------------------------------- |
| `OBJECT_INHERIT_ACE`         | 0x01  | Child files (non-containers) inherit this ACE              |
| `CONTAINER_INHERIT_ACE`      | 0x02  | Child directories (containers) inherit this ACE            |
| `NO_PROPAGATE_INHERIT_ACE`   | 0x04  | Inherited copy clears `OI`/`CI` on the child               |
| `INHERIT_ONLY_ACE`           | 0x08  | ACE does not apply to the object itself, only to children  |
| `INHERITED_ACE`              | 0x10  | This ACE was propagated from a parent (vs set explicitly)  |

The security descriptor's `Control` field carries inheritance state at
the descriptor level (see `SECURITY_DESCRIPTOR_CONTROL` in `winnt.h`):

| Flag                            | Value  | Meaning                                                            |
| ------------------------------- | ------ | ------------------------------------------------------------------ |
| `SE_DACL_AUTO_INHERITED` (`AI`) | 0x0400 | DACL was built using automatic inheritance from parent             |
| `SE_DACL_PROTECTED` (`P`)       | 0x1000 | DACL is protected from inheritance; parent ACEs are not propagated |
| `SE_DACL_AUTO_INHERIT_REQ`      | 0x0100 | Request to compute inheritance on the next ACL modification        |

### 2.2 Behavioural contract

The Win32 access-check engine treats the two categories very
differently:

- **Explicit ACEs** are persisted on the object. They survive parent
  DACL changes.
- **Inherited ACEs** are recomputed from the parent's inheritable ACE
  set whenever the parent's DACL changes (provided
  `SE_DACL_PROTECTED` is not set on the child).

A backup or sync tool that re-writes an inherited ACE as an explicit
ACE silently breaks the inheritance chain: the destination's DACL is
frozen at the source's state at the moment of transfer and no longer
tracks parent updates. Microsoft documents the desired round-trip
behaviour under `SetSecurityInfo`'s
`UNPROTECTED_DACL_SECURITY_INFORMATION` flag and the
`SE_DACL_AUTO_INHERITED` control bit. Backup APIs
(`BackupRead`/`BackupWrite`) preserve `AceFlags` verbatim, including
`INHERITED_ACE`.

## 3. Inventory of current DACL handling

All Windows DACL code lives under `crates/metadata/src/acl_windows/`
behind the `cfg(all(feature = "acl", windows))` gate. The wire glue
lives in `crates/protocol/src/acl/`. Hits below are grouped by what
they do.

### 3.1 ACE / SD flag awareness

| File                                                                              | Construct                              | Flag inspection                                                                                                                            |
| --------------------------------------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/metadata/src/acl_windows/dacl.rs:14-22`                                   | imports                                | Brings in `ACE_HEADER`, `ACL`, `ACCESS_ALLOWED_ACE`. **Does not import** `INHERITED_ACE`, `OBJECT_INHERIT_ACE`, or any `SE_DACL_*` constant |
| `crates/metadata/src/acl_windows/dacl.rs:166-209` (`dacl_to_rsync_acl`)           | ACE walk                               | Reads `header.AceType` only. `header.AceFlags` is never touched                                                                            |
| `crates/metadata/src/acl_windows/dacl.rs:178`                                     | filter                                 | `if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 { continue; }` drops deny / audit / object ACEs. No flag-bit branch                    |
| `crates/metadata/src/acl_windows/dacl.rs:332-440` (`apply_rsync_acl_to_path`)     | write side                             | Uses `InitializeAcl` + `AddAccessAllowedAce`. `AddAccessAllowedAce` always emits `AceFlags == 0`, so every ACE is explicit                 |
| `crates/metadata/src/acl_windows/dacl.rs:418-427`                                 | `SetNamedSecurityInfoW` call           | Info mask is `DACL_SECURITY_INFORMATION` only. No `PROTECTED_DACL_SECURITY_INFORMATION` / `UNPROTECTED_DACL_SECURITY_INFORMATION` opt-in   |
| `crates/metadata/src/acl_windows/sddl.rs:17-22`                                   | imports                                | Brings in `PROTECTED_DACL_SECURITY_INFORMATION` but no `UNPROTECTED_DACL_*` or `SE_DACL_AUTO_INHERITED`                                    |
| `crates/metadata/src/acl_windows/sddl.rs:250-251`                                 | write-side info bits                   | Forces `PROTECTED_DACL_SECURITY_INFORMATION` whenever any DACL is present, regardless of the source descriptor's `AI` state                |
| `crates/metadata/src/acl_windows/sddl.rs:357-384` (`split_sddl`)                  | SDDL section splitter                  | Strips leading section flags. Doc comment at `:356` and `:398` acknowledges `P` / `AI` but the helper discards them when reading           |
| `crates/metadata/src/acl_windows/sddl.rs:386-420` (`parse_aces`, `ParsedAce`)     | per-ACE parse                          | Captures `ace_type`, `flags`, `rights`, `trustee`. `flags` is a `&str`, never decoded to per-bit flags                                     |
| `crates/metadata/src/acl_windows/posix_map.rs:54-58`                              | inheritance check                      | `if ace.flags.contains("ID") { dropped = true; continue; }` - drops inherited ACEs from POSIX mode mapping; preservation is not the goal   |
| `crates/metadata/src/acl_windows/posix_map.rs:117-130` (`posix_mode_to_dacl`)     | emit                                   | Always emits `D:P(...)` (protected) with no `AI`; produces only explicit ACEs                                                              |

### 3.2 Win32 security-info entry points used

| File                                                | Win32 API                                                | Information mask                                                                                                                              |
| --------------------------------------------------- | -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/metadata/src/acl_windows/dacl.rs:49-60`     | `GetNamedSecurityInfoW`                                  | `DACL_SECURITY_INFORMATION`                                                                                                                   |
| `crates/metadata/src/acl_windows/dacl.rs:417-427`   | `SetNamedSecurityInfoW`                                  | `DACL_SECURITY_INFORMATION`                                                                                                                   |
| `crates/metadata/src/acl_windows/sddl.rs:94-105`    | `GetNamedSecurityInfoW` (SDDL path)                      | `OWNER | GROUP | DACL` plus optional `SACL`                                                                                                   |
| `crates/metadata/src/acl_windows/sddl.rs:273-283`   | `SetNamedSecurityInfoW` (SDDL path)                      | `OWNER | GROUP | DACL | PROTECTED_DACL_SECURITY_INFORMATION` plus optional `SACL`. **Never `UNPROTECTED_DACL_SECURITY_INFORMATION`**           |
| `crates/metadata/src/acl_windows/sddl.rs:119-127`   | `ConvertSecurityDescriptorToStringSecurityDescriptorW`   | Owner/group/DACL/SACL info mask; output preserves `AI` and per-ACE `ID` per Microsoft SDDL grammar                                            |
| `crates/metadata/src/acl_windows/sddl.rs:177-184`   | `ConvertStringSecurityDescriptorToSecurityDescriptorW`   | Parses the SDDL string back into a binary SD; binary SD carries `AceFlags` and SD `Control` faithfully, but the write side then ignores them  |

There are **no** call sites for `GetKernelObjectSecurity`,
`SetKernelObjectSecurity`, `GetSecurityDescriptorControl`,
`SetSecurityDescriptorControl`, or `BackupRead` / `BackupWrite` in the
metadata crate.

## 4. Read-side audit

The DACL read path has two branches:

1. **`get_rsync_acl` + `dacl_to_rsync_acl`** at
   `crates/metadata/src/acl_windows/dacl.rs:146-209` walks each ACE in
   the kernel-returned `ACL` structure. The ACE-header read at
   `dacl.rs:174-176` casts `ace_ptr` to `*const ACE_HEADER` and reads
   `header.AceType` only. `header.AceFlags` is loaded into the
   structure (it is part of `ACE_HEADER`) but it is never inspected or
   stored. The downstream `RsyncAcl` representation
   (`crates/protocol/src/acl/entry.rs`) has no per-entry flag slot, so
   even if the read side captured the bit it would have nowhere to
   put it.

2. **`read_dacl_sddl`** at
   `crates/metadata/src/acl_windows/sddl.rs:70-145` extracts a full SD
   and asks `ConvertSecurityDescriptorToStringSecurityDescriptorW`
   (`sddl.rs:119-127`) to serialise it. The SDDL string emitted by the
   Win32 converter preserves the per-ACE inheritance flag as the
   two-letter token `ID` and the descriptor-level
   `SE_DACL_AUTO_INHERITED` bit as the `AI` section flag. So the SDDL
   payload itself does carry the inheritance state when it is
   produced.

The descriptor-level `Control` field is **not** read explicitly. The
SDDL path captures `AI` indirectly through the converter; the
named-ACE path captures nothing.

**Read-side conclusion:** the `dacl_to_rsync_acl` path drops the
`INHERITED_ACE` bit silently; the SDDL path preserves it on the wire
but throws it away on the write side (section 6 below).

## 5. Wire encoding

Two wire formats are in play.

### 5.1 Named-ACE wire (`-A` / `--acls` default)

The rsync ACL wire format lives at
`crates/protocol/src/acl/wire/send.rs` and `recv.rs`. The encoding is
identical to upstream rsync's POSIX-style `acls.c`:

- `crates/protocol/src/acl/wire/send.rs:39-64` (`send_ida_entries`)
  encodes each named entry as `(id, access, [name])`. The `access`
  varint at `send.rs:50` carries only `(perms << 2) | flags`, where
  `perms` is a 3-bit rwx triplet and `flags` are
  `XFLAG_NAME_FOLLOWS` / `XFLAG_NAME_IS_USER`.
- `crates/protocol/src/acl/wire/send.rs:87-133` (`send_rsync_acl`)
  emits `XMIT_*` flags (`USER_OBJ`, `GROUP_OBJ`, `MASK_OBJ`,
  `OTHER_OBJ`, `NAME_LIST`) followed by base triplets and the named
  list. No slot exists for per-ACE Windows flag bits, deny ACEs,
  audit ACEs, or the SD `Control` field.
- `crates/protocol/src/acl/wire/encoding.rs:23-35` (`encode_access`)
  confirms the access varint layout is exactly perms + 2 flag bits.

Conclusion: the default named-ACE wire is structurally incapable of
carrying the `INHERITED_ACE` bit. This matches the design doc note at
`docs/design/windows-ntfs-acl-support.md:33` ("SIDs are not
transmitted; hardlink double-application is not guarded") and the
upstream-compatible lossy semantics flagged by
`crates/metadata/src/acl_windows/mod.rs:24-43`.

### 5.2 SDDL xattr wire (`WAS-6` opt-in)

The richer wire form goes through the xattr channel:

- `crates/metadata/src/acl_windows/xattr.rs:20` declares the reserved
  slot `user.win32.security_descriptor` (mirroring Samba's
  convention).
- `crates/metadata/src/acl_windows/xattr.rs:35-50` (`sddl_xattr_entry`)
  reads the source SDDL via `read_dacl_sddl` and packs it into an
  `XattrEntry`. Because the SDDL string includes section flags (`P`,
  `AI`) and per-ACE flags (`ID`, `OI`, `CI`, `IO`, `NP`), the wire
  payload carries the inheritance metadata verbatim.
- `crates/transfer/src/generator/file_list/entry.rs:230` is the
  generator-side wiring that injects the entry into the file list's
  xattr block.
- `crates/metadata/src/acl_windows/xattr.rs:79-93`
  (`apply_sddl_from_xattrs`) feeds the received SDDL straight into
  `write_dacl_sddl`.

Conclusion: the SDDL xattr wire **does** preserve `INHERITED_ACE`
and `SE_DACL_AUTO_INHERITED` in transit. The fidelity gap lies in how
`write_dacl_sddl` consumes the payload at the receiver - see section
6.2.

## 6. Write-side audit

### 6.1 Named-ACE write path

`apply_rsync_acl_to_path` at
`crates/metadata/src/acl_windows/dacl.rs:332-440` reconstructs a fresh
DACL:

1. `InitializeAcl(dacl_buf, dacl_size, ACL_REVISION)` at
   `dacl.rs:382-391` allocates an empty ACL with no inheritance state.
2. For each surviving named entry, `AddAccessAllowedAce(...)` at
   `dacl.rs:399-412` appends a new ACE. The `AddAccessAllowedAce`
   Win32 API sets `AceFlags = 0` unconditionally; no overload of this
   helper exposes the `INHERITED_ACE` bit. The richer
   `AddAccessAllowedAceEx` (which accepts an `AceFlags` argument) is
   not used.
3. `SetNamedSecurityInfoW(..., DACL_SECURITY_INFORMATION, None, None,
   Some(dacl_buf), None)` at `dacl.rs:418-427` applies the new DACL
   without `PROTECTED_DACL_SECURITY_INFORMATION` or
   `UNPROTECTED_DACL_SECURITY_INFORMATION`. The OS defaults to the
   object's existing protection state.

Net effect: every transferred ACE lands on the destination as
**explicit** (`AceFlags == 0`). Any ACE the source had marked
`INHERITED_ACE` is silently re-materialised as explicit. The
inheritance chain at the destination is broken.

### 6.2 SDDL write path

`write_dacl_sddl` at
`crates/metadata/src/acl_windows/sddl.rs:170-289` does most of the
right thing but loses one critical bit:

1. `ConvertStringSecurityDescriptorToSecurityDescriptorW` at
   `sddl.rs:177-184` parses the SDDL into a binary SD. The binary SD
   carries per-ACE `AceFlags` (including `INHERITED_ACE`) and the SD
   `Control` field (including `SE_DACL_AUTO_INHERITED`) faithfully.
2. `GetSecurityDescriptorDacl` at `sddl.rs:220-223` extracts the DACL
   pointer.
3. `SetNamedSecurityInfoW` at `sddl.rs:273-283` is called with the
   information mask `OWNER | GROUP | DACL |
   PROTECTED_DACL_SECURITY_INFORMATION` (line 251) regardless of what
   the source descriptor said.

The unconditional `PROTECTED_DACL_SECURITY_INFORMATION` is the killer:
it tells the kernel "do not inherit anything from the parent", which
sets `SE_DACL_PROTECTED` on the destination and clears
`SE_DACL_AUTO_INHERITED`. Even when the SDDL payload contains `AI`
section flag, the receiver overwrites it with `P`.

Worse, when `SetNamedSecurityInfoW` is told the DACL is protected, the
kernel still copies the per-ACE `INHERITED_ACE` bits from the supplied
ACL buffer verbatim. So the destination ends up in a self-contradicting
state: a protected DACL (no parent linkage) that contains ACEs marked
as inherited. Windows tooling (Explorer's Security tab,
`icacls /verify`) reports these as anomalous.

The doc comment at `sddl.rs:150-153` documents this as deliberate:

> The DACL is applied with `PROTECTED_DACL_SECURITY_INFORMATION` so the
> destination does not silently inherit additional ACEs from its parent,
> matching the policy laid out in
> `docs/design/windows-ntfs-acl-support.md` section 5.2.

That policy is a defensible default for cross-platform transfers
(POSIX -> Windows) where there is no source inheritance state to honour.
It is incorrect for Windows-to-Windows transfers where the source SD
already encodes the right answer.

### 6.3 POSIX-mode-derived write

`posix_mode_to_dacl` at
`crates/metadata/src/acl_windows/posix_map.rs:117-130` always emits
`D:P(...)` (protected) with no `AI`. This is correct for POSIX-derived
data where there is nothing to inherit. No change needed here.

## 7. Existing test coverage

The metadata crate has Windows ACL tests in
`crates/metadata/src/acl_windows/tests/`. None of them exercises the
parent-directory inheritance round trip.

| Test                                                                                            | What it covers                                                            | Inheritance check?                                                            |
| ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `tests/dacl.rs:96-109` `read_dacl_on_temp_file_returns_dacl`                                    | Smoke-tests `read_dacl` returns a non-null pointer on an NTFS temp file   | No                                                                            |
| `tests/dacl.rs:19-77`                                                                           | `reconstruct_acl` / `apply_acls_from_cache` happy paths and no-op branches | No                                                                            |
| `tests/sddl.rs:71-114` `read_dacl_sddl_returns_non_empty_for_temp_file`, `write_dacl_sddl_round_trips_known_descriptor` | SDDL serialise / parse round-trip on a flat descriptor with `D:P(...)` (protected) | No - the payload is hard-coded protected, no `AI`, no `ID` ACEs       |
| `tests/sddl.rs:116-141`                                                                         | Owner/group preservation through SDDL                                     | No                                                                            |
| `tests/sync.rs:29-66` `sync_acls_round_trips_on_ntfs`, `sync_acls_prefers_sddl_round_trip`      | End-to-end `sync_acls` between two temp files                             | No - the source is a freshly created NTFS file with whatever inherited ACEs the temp dir provides; the test asserts no error and four substring matches, none of which constrain inheritance state |
| `tests/posix_map.rs:54-58` `dacl_to_posix_mode_drops_inherited_aces`                            | Asserts the POSIX-mode mapping drops ACEs whose SDDL flags contain `ID`   | Tests the **drop** semantics (correct for cross-platform), not preservation   |
| `crates/metadata/tests/windows_to_linux_acl_roundtrip.rs`                                       | Sends a fixed SDDL payload through the xattr wire path                    | No - hard-coded `D:P` SDDL with explicit ACEs only                            |

The integration-test layer has the same shape: no test constructs a
parent directory with inheritable ACEs (`OI` / `CI`), seeds a child
under it, transfers the child via oc-rsync, and asserts the
destination child's DACL keeps the inherited entries marked with
`INHERITED_ACE` and the SD-level `SE_DACL_AUTO_INHERITED` bit set.

**Test gap:** there is no regression test pinning inheritance fidelity.

## 8. Findings

- **F1: read-side per-ACE inheritance inspection** - **No.**
  `dacl_to_rsync_acl` at
  `crates/metadata/src/acl_windows/dacl.rs:166-209` reads only
  `header.AceType` (`dacl.rs:178`) and does not inspect
  `header.AceFlags`. The `RsyncAcl` slot the result lands in
  (`crates/protocol/src/acl/entry.rs`) has no per-entry flag field. The
  SDDL path (`crates/metadata/src/acl_windows/sddl.rs:70-145`) does
  capture inheritance state indirectly through the Win32 SDDL
  converter, but only because the SDDL string format encodes `ID` and
  `AI` itself.

- **F2: wire preserves the `INHERITED_ACE` bit** - **Mixed.** The
  default named-ACE wire at `crates/protocol/src/acl/wire/send.rs:39-133`
  structurally cannot carry per-ACE Windows flag bits: the `access`
  varint at `send.rs:50` and `wire/encoding.rs:23-35` is a 3-bit rwx
  triplet plus two `XFLAG_*` flags. The opt-in SDDL xattr at
  `crates/metadata/src/acl_windows/xattr.rs:20-50` (slot
  `user.win32.security_descriptor`) does carry `ID` and `AI` verbatim
  because the SDDL grammar encodes them.

- **F3: write-side restores ACEs as inherited (vs flattens to
  explicit)** - **No.** Both write paths flatten inheritance:

  - `apply_rsync_acl_to_path`
    (`crates/metadata/src/acl_windows/dacl.rs:332-440`) builds a new
    DACL with `InitializeAcl` + `AddAccessAllowedAce`. Every ACE lands
    with `AceFlags == 0` (`dacl.rs:399-412`). `AddAccessAllowedAceEx`
    is not used.
  - `write_dacl_sddl`
    (`crates/metadata/src/acl_windows/sddl.rs:170-289`) parses the SD
    correctly but then unconditionally forces
    `PROTECTED_DACL_SECURITY_INFORMATION` (`sddl.rs:251`), overriding
    the source's `AI` state and leaving the destination DACL in a
    self-contradicting protected-but-marked-inherited posture.

  Concrete reproduction: source DACL `{inherited_ace_A,
  explicit_ace_B}` arrives at the destination as either
  `{explicit_ace_A, explicit_ace_B}` (named-ACE wire path) or
  `{inherited-flagged_ace_A on a protected DACL, explicit_ace_B}`
  (SDDL xattr path). Neither matches the source.

- **F4: `SE_DACL_AUTO_INHERITED` control-bit round trip** - **No.**
  No call site reads or writes the descriptor's `Control` field.
  `GetSecurityDescriptorControl` / `SetSecurityDescriptorControl` are
  absent. The SDDL write path forces `SE_DACL_PROTECTED` at
  `crates/metadata/src/acl_windows/sddl.rs:251`. The named-ACE write
  path leaves the control bits at whatever the destination object
  already had, so the bit is not "round-tripped" so much as ignored.

- **F5: regression-test coverage** - **Absent.** Section 7 enumerates
  every existing Windows DACL test. None constructs a parent
  directory with inheritable ACEs, places a child under it, transfers
  the child through oc-rsync, and asserts the destination child's
  DACL still marks the inherited entries with `INHERITED_ACE`. The
  closest existing test
  (`tests/posix_map.rs:54-58 dacl_to_posix_mode_drops_inherited_aces`)
  asserts the **drop** semantics for cross-platform mapping, not
  preservation.

## 9. Risk surface

If F3 / F4 remain unfixed, three failure modes are reachable from
real-world workloads:

1. **DACL drift over time.** The destination's DACL is frozen at the
   moment of transfer. If an administrator subsequently changes the
   parent directory's inheritable ACEs on the destination (e.g. adds
   a new "AuditTeam Read" ACE to all data directories), the
   transferred children do not pick it up, while siblings created
   natively do. The longer the destination is administered
   independently, the wider the gap.

2. **Subtle privilege escalation / leak.** Suppose the source has an
   inherited *deny* ACE blocking a deprecated group. On a Windows-to-
   Windows SDDL transfer, the deny ACE is dropped during ACL walk
   (`dacl.rs:178-183` drops non-allow types) but the parent's allow
   ACEs still flatten through to the destination as explicit. Future
   parent-level removal of those allow ACEs has no effect on the
   destination. Net result: the destination grants access the source
   no longer would.

3. **Tooling inconsistency.** `icacls /verify`, Explorer's "Effective
   Access" tab, and Group Policy desired-state-configuration scanners
   inspect the `AI` / `ID` markers. A DACL that contains
   inheritance-marked ACEs on a protected SD (the SDDL path output
   described in F3) is reported as anomalous and may trigger
   compliance flags on hardened deployments.

The audit does not produce concrete data showing any of these has
been hit by an oc-rsync user, but the failure mode is intrinsic to the
current code rather than dependent on environment.

## 10. Recommendations

WPC-10 ships only the audit; the following recommendations are
candidate follow-up tasks. Each is independently shippable.

- **R1 - Preserve `INHERITED_ACE` on the write side.** Replace
  `AddAccessAllowedAce` at
  `crates/metadata/src/acl_windows/dacl.rs:399-412` with
  `AddAccessAllowedAceEx`, which accepts an `AceFlags` argument. To
  carry the flag across the wire, either:

  - Extend the rsync named-ACE wire with an additional varint per
    `IdAccess` entry holding the Windows `AceFlags` byte, or
  - Make the SDDL xattr the canonical Windows-to-Windows path and
    route the named-ACE wire only when the receiver advertises POSIX
    semantics.

  The wire extension violates the
  `[[feedback_no_wire_protocol_features]]` rule, so the SDDL-xattr
  route is the only realistic answer. R1 reduces in practice to
  routing decisions plus the R2 SD-control fix.

- **R2 - Honour `SE_DACL_AUTO_INHERITED` on the SDDL write path.**
  At `crates/metadata/src/acl_windows/sddl.rs:170-289`:

  1. After `ConvertStringSecurityDescriptorToSecurityDescriptorW` at
     `sddl.rs:177-184`, call `GetSecurityDescriptorControl` on
     `owned.pd` to read the source's `Control` field.
  2. If `SE_DACL_PROTECTED` is set on the source, retain the current
     behaviour at `sddl.rs:251` (force `PROTECTED_DACL_SECURITY_INFORMATION`).
  3. If `SE_DACL_AUTO_INHERITED` is set on the source, replace the
     current `PROTECTED_DACL_SECURITY_INFORMATION` at `sddl.rs:251`
     with `UNPROTECTED_DACL_SECURITY_INFORMATION` so the kernel
     recomputes the inherited portion from the destination's parent.

  This restores correct behaviour for both protected and
  auto-inherited source SDs. The doc comment at `sddl.rs:150-153` and
  `docs/design/windows-ntfs-acl-support.md` section 5.2 need updating
  to reflect the new policy.

- **R3 - Add an inheritance round-trip regression test.** Under
  `crates/metadata/src/acl_windows/tests/` (or as a new file under
  `crates/metadata/tests/`):

  1. Create a parent directory and seed its DACL with an inheritable
     allow ACE: SDDL fragment
     `(A;OICI;FRFX;;;BU)` (BUILTIN\\Users gets read+execute,
     `OI` / `CI` inheritance flags set).
  2. Create a child file under the parent. The kernel materialises
     the inheritable ACE on the child with `AceFlags = INHERITED_ACE
     | (the inheritance bits the parent ACE had stripped via NP)`.
     Verify with `GetNamedSecurityInfoW` that the child's DACL
     contains an ACE with `INHERITED_ACE` set.
  3. Run the child through `sync_acls` (and separately through the
     SDDL xattr path).
  4. Assert that the destination child's DACL still contains an ACE
     for the same trustee with `INHERITED_ACE` set, and that the
     destination's `Control` field has `SE_DACL_AUTO_INHERITED` set
     iff the source did.

  Gate with `#[cfg(windows)]`. The test does not require admin
  privileges because it operates on a `tempdir`-scoped tree.

## 11. Cross-references

- **WAS-1 / WAS-2 / WAS-3 / WAS-4 / WAS-5** (SID mapping, named-ACE
  wire, deny / inherited diagnostics, cross-platform translation
  matrix, hardlink-safe DACL application). See
  `docs/design/windows-ntfs-acl-support.md` section 8 and the
  `acl_windows::dacl` module.
- **WAS-6** (#2311) - SDDL fidelity payload (the
  `user.win32.security_descriptor` xattr slot). Parallel to this
  audit: the xattr carries inheritance markers but the receiver
  drops them. See `crates/metadata/src/acl_windows/xattr.rs` and
  section 6.2 above.
- **WAS-7 / WAS-8** (SACL audit ACE support, Windows-to-POSIX bulk
  fidelity). See
  `docs/design/windows-ntfs-acl-support.md:309` and the
  `acl_windows::sddl::read_sddl_with_sacl` entry point.
- **WPC-13** (#2915, PR #4920) - Windows support matrix. The
  inherited-vs-explicit gap is the row at
  `docs/user/windows-support-matrix.md:97-100` referencing WPC-10.
- **Memory notes:** `[[project_windows_real_world_parity_unclear]]`
  (flags this audit as the open WPC slot for ACL inheritance) and
  `[[project_windows_parity_wip]]` (the WPC umbrella).
- **Upstream reference:** Microsoft, "How AutoInheritance Works",
  documented under `ConvertToAutoInheritPrivateObjectSecurity` and
  `SetNamedSecurityInfo` (`UNPROTECTED_DACL_SECURITY_INFORMATION` in
  `windows-sys`). Upstream rsync's Cygwin ACL path (`acls.c`) does
  not consult Windows inheritance flags at all; oc-rsync's SDDL
  xattr is intentionally richer than upstream.
