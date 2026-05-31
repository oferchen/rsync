# Per-direction xattr/ACL gap-fix tasks (XAP-11)

Parent series: XAP (xattr/ACL cross-platform parity). Tracks
`[[project_xattr_acl_cross_platform_parity_gap]]`.

## 1. Purpose

XAP-4 through XAP-7 specified round-trip validation for each
source-destination platform pair. XAP-9 synthesized the results into a
direction matrix with per-cell validation status. This document (XAP-11)
catalogues the specific gaps found, proposes concrete fix tasks for each,
estimates complexity, and assigns priority based on real-world user
impact.

## 2. XAP-4 through XAP-7 round-trip outcome summary

### XAP-4: Linux cross-platform round-trip tests

| Cell | Xattr | ACL | Notes |
|------|-------|-----|-------|
| Linux -> Linux | PASS | PASS | Full POSIX.1e + NFSv4 + namespaced xattr. Validated by interop tests against upstream 3.4.1/3.4.2. |
| Linux -> macOS | UNTESTED | UNTESTED | Spec exists (XAP-1 section 2). Requires macOS + Linux endpoints. |
| Linux -> Windows | UNTESTED | UNTESTED | Simulated mapping tests only (`acl_windows_to_linux_roundtrip.rs`). No real NTFS endpoint exercised. |

### XAP-5: macOS -> Linux round-trip tests

| Cell | Xattr | ACL | Notes |
|------|-------|-----|-------|
| macOS -> Linux | PARTIAL | UNTESTED | XAP-8 confirmed `com.apple.*` bytes round-trip. Resource fork > 64 MiB truncation (F2). No automated cross-host test. |
| macOS -> macOS | PARTIAL | PARTIAL | Gated interop test covers xattrs including resource forks. ACL test exists but no upstream-parity assertion. |

### XAP-6: Windows cross-platform round-trip tests

| Cell | Xattr | ACL | Notes |
|------|-------|-----|-------|
| Windows -> Linux | UNTESTED | UNTESTED | Simulated DACL-to-POSIX mapping test only. No live Windows source exercised. |
| Windows -> Windows | UNTESTED | UNTESTED | DACL inheritance audit (WPC-10) found write-side flattens all ACEs to explicit. |
| Linux -> Windows | UNTESTED | UNTESTED | No real NTFS endpoint. |

### XAP-7: Windows <-> macOS round-trip tests

| Cell | Xattr | ACL | Notes |
|------|-------|-----|-------|
| macOS -> Windows | UNTESTED | UNTESTED | XAP-8 finding: colon-bearing `com.apple.metadata:*` names mangle on Windows ADS. |
| Windows -> macOS | UNTESTED | UNTESTED | Same DACL-to-POSIX collapse as Windows -> Linux. |

## 3. Per-direction gap list

### Gap G1: Resource fork > 64 MiB truncation

- **Directions affected:** macOS -> any destination (macOS, Linux, Windows).
- **Root cause:** The `xattr` crate makes a single `getxattr(2)` call
  with `position=0`. macOS caps a single call at 64 MiB for resource
  forks. Upstream rsync loops with rising `position` arguments
  (`lib/sysxattrs.c:60-80`).
- **Symptom:** Silent truncation of resource fork data beyond 64 MiB.
- **Severity:** Low (rare in practice - only legacy Carbon media files).
- **Source ref:** XAP-8 finding F2, `crates/metadata/src/xattr_unix.rs:30-41`.

### Gap G2: Quarantine xattr preserved on backup restore

- **Directions affected:** macOS -> macOS, macOS -> Linux -> macOS
  (restore path).
- **Root cause:** Neither oc-rsync nor upstream filters
  `com.apple.quarantine`. Gatekeeper re-flags restored files.
- **Symptom:** Gatekeeper prompts on files restored from backup.
- **Severity:** Medium (affects macOS backup workflows).
- **Source ref:** XAP-8 finding F3, recommendation R1.

### Gap G3: Colon-in-xattr-name collision on Windows ADS

- **Directions affected:** macOS -> Windows, Linux -> Windows (when
  xattr name contains `:`).
- **Root cause:** `stream_path_wide` in
  `crates/metadata/src/xattr_windows.rs` builds ADS paths by literal
  concatenation. Win32 treats `:` as the stream-name separator.
  `com.apple.metadata:_kMDItemUserTags` becomes
  `file:com.apple.metadata:_kMDItemUserTags:$DATA` which Win32 parses
  as stream `com.apple.metadata` of type `_kMDItemUserTags`.
- **Symptom:** Xattr name mangled; data written to wrong stream name.
  Subsequent Windows -> macOS transfer does not restore the original name.
- **Severity:** Medium (affects Finder tags and download-origin metadata).
- **Source ref:** XAP-8 section 7.4, WPC-1 audit (`docs/audit/windows-ads-handling.md`).

### Gap G4: DACL inheritance flattened to explicit ACEs

- **Directions affected:** Windows -> Windows (SDDL and named-ACE paths).
- **Root cause:** Named-ACE path uses `AddAccessAllowedAce` (always
  `AceFlags=0`). SDDL path forces `PROTECTED_DACL_SECURITY_INFORMATION`
  unconditionally, overriding `SE_DACL_AUTO_INHERITED`.
- **Symptom:** Destination DACL breaks inheritance chain. Parent DACL
  changes no longer propagate. Tooling reports anomalous SD state.
- **Severity:** High (affects Windows domain deployments using ACL
  inheritance for policy propagation).
- **Source ref:** WPC-10 findings F1-F5,
  `crates/metadata/src/acl_windows/dacl.rs:332-440`,
  `crates/metadata/src/acl_windows/sddl.rs:170-289`.

### Gap G5: ADS namespace filtering on Linux receiver

- **Directions affected:** Windows -> Linux.
- **Root cause:** ADS stream names from a Windows source land as xattrs
  only if the name carries the `user.` prefix. Names without it are
  rejected by `is_xattr_permitted` for non-root receivers.
- **Symptom:** Non-`user.`-prefixed ADS streams silently dropped on
  non-root Linux receivers.
- **Severity:** Low (matches upstream behaviour; Windows users rarely
  prefix ADS names with `user.`).
- **Source ref:** XAP-1 section 7, XAP-9 cell 7.

### Gap G6: POSIX default ACLs dropped on macOS/Windows

- **Directions affected:** Linux -> macOS, Linux -> Windows.
- **Root cause:** macOS HFS+/APFS and Windows NTFS have no concept of
  POSIX directory default ACLs. Structural mismatch - no fix possible.
- **Symptom:** New files created under transferred directories do not
  inherit source ACL policy.
- **Severity:** Medium (affects users relying on default ACL propagation).
- **Source ref:** XAP-1 sections 2, 3; cannot be fixed without wire
  protocol extension.

### Gap G7: macOS deny/audit/alarm ACEs dropped on Linux

- **Directions affected:** macOS -> Linux.
- **Root cause:** POSIX.1e has no deny, audit, or alarm ACE types.
  Structural mismatch.
- **Symptom:** macOS deny rules not enforced on Linux destination.
- **Severity:** Low-Medium (deny ACEs are uncommon in macOS deployments).
- **Source ref:** XAP-1 section 4.

### Gap G8: Named-user/group ACEs unresolvable on Windows

- **Directions affected:** Linux -> Windows, macOS -> Windows.
- **Root cause:** POSIX named-user/named-group ACEs require
  `LookupAccountNameW` resolution on the Windows receiver. Principals
  without a matching Windows account are dropped.
- **Symptom:** Named ACL entries silently lost.
- **Severity:** Medium (affects cross-platform environments with
  non-overlapping user databases).
- **Source ref:** XAP-1 section 3,
  `crates/metadata/src/acl_windows/posix_map.rs`.

### Gap G9: Linux -> macOS and macOS -> Linux round-trip tests absent

- **Directions affected:** Linux <-> macOS (both directions).
- **Root cause:** No CI infrastructure exercises cross-host transfers.
  The gated interop test (`OC_RSYNC_METADATA_INTEROP=1`) only runs on
  a single platform.
- **Symptom:** Regressions in cross-platform metadata handling go
  undetected until a user reports them.
- **Severity:** High (test coverage gap masks defects).
- **Source ref:** XAP-5 (pending), XAP-9 gaps table.

### Gap G10: All Windows-involved directions untested on real hardware

- **Directions affected:** Any -> Windows, Windows -> Any, Windows ->
  Windows.
- **Root cause:** No CI runner with NTFS, real SIDs, and the metadata
  crate's Windows surface exercised.
- **Symptom:** Windows ACL/xattr code paths validated only by simulated
  unit tests with hardcoded payloads.
- **Severity:** High (entire Windows metadata surface is integration-test-blind).
- **Source ref:** XAP-6, XAP-7, #1869 (CI matrix doc).

## 4. Proposed fix tasks

### Task T1: Chunked resource-fork read (closes G1)

Implement the upstream `getxattr(2)` loop with rising `position`
arguments for resource forks exceeding 64 MiB.

- **Location:** `crates/metadata/src/xattr_unix.rs:30-41` (or a new
  `macos_chunked_read` helper).
- **Approach:** Detect macOS + resource fork name, loop with 64 MiB
  chunks until kernel returns fewer bytes than requested.
- **Complexity:** Low (20-40 lines, single function).
- **Test:** Create a synthetic 65 MiB resource fork in a temp file,
  round-trip through oc-rsync, assert byte-for-byte match.
- **Platform constraint:** macOS only (`#[cfg(target_os = "macos")]`).

### Task T2: `--macos-strip-quarantine` receiver flag (closes G2)

Add an opt-in receiver-side flag that drops `com.apple.quarantine` from
the xattr stream during apply.

- **Location:** `crates/metadata/src/xattr.rs` (apply path), CLI
  option in `crates/cli/src/options/`.
- **Approach:** Check the flag in `apply_xattrs_from_list`; skip any
  entry whose name equals `com.apple.quarantine`.
- **Complexity:** Low (flag plumbing + 5-line filter).
- **Test:** Transfer a quarantined file with the flag enabled, assert
  the destination has no quarantine xattr.
- **Platform constraint:** Receiver must be macOS for the flag to be
  meaningful (no-op elsewhere).

### Task T3: Colon-escaping for ADS stream names (closes G3)

Escape colons in xattr names before building the ADS path on Windows.
Unescape on the read side so the original name survives a round-trip.

- **Location:** `crates/metadata/src/xattr_windows.rs`
  (`stream_path_wide`, `list_attributes`).
- **Approach:** Replace `:` with a reversible escape sequence (e.g.
  `%3A` or a Windows-safe substitute character). Document the encoding
  in the module-level rustdoc. Preserve backward compatibility by
  detecting unescaped names on read.
- **Complexity:** Medium (escape/unescape logic + backward compat +
  path-length implications).
- **Test:** Round-trip `com.apple.metadata:_kMDItemUserTags` through a
  Windows transfer, assert the xattr name is restored exactly.
- **Platform constraint:** Windows only. Must not exceed MAX_PATH for
  the combined `file:escaped_name:$DATA` path.

### Task T4: Honour DACL inheritance state on SDDL write (closes G4)

Respect the source SD's `SE_DACL_AUTO_INHERITED` / `SE_DACL_PROTECTED`
control bits when applying the DACL via the SDDL xattr path.

- **Location:** `crates/metadata/src/acl_windows/sddl.rs:170-289`.
- **Approach:** After parsing SDDL to binary SD, read the `Control`
  field via `GetSecurityDescriptorControl`. If `SE_DACL_PROTECTED` is
  set, retain current `PROTECTED_DACL_SECURITY_INFORMATION`. If
  `SE_DACL_AUTO_INHERITED` is set, use
  `UNPROTECTED_DACL_SECURITY_INFORMATION` instead. Update
  `AddAccessAllowedAce` -> `AddAccessAllowedAceEx` in the named-ACE
  path to preserve `AceFlags`.
- **Complexity:** Medium-High (requires careful testing on real NTFS;
  the named-ACE path needs the `RsyncAcl` type extended or routed
  through SDDL xattr exclusively for Windows-to-Windows).
- **Test:** WPC-10 recommendation R3: create parent with inheritable
  ACE, transfer child, assert `INHERITED_ACE` flag and
  `SE_DACL_AUTO_INHERITED` preserved.
- **Platform constraint:** Windows only. Named-ACE wire cannot carry
  `AceFlags` without a protocol extension (ruled out per project
  policy). Fix applies only to the SDDL xattr path for
  Windows-to-Windows transfers.

### Task T5: macOS <-> Linux automated CI round-trip (closes G9)

Wire the XAP-2 and XAP-3 harness primitives into a cross-platform CI
job that exercises Linux -> macOS and macOS -> Linux in both xattr and
ACL dimensions.

- **Location:** `.github/workflows/ci.yml` (new job or matrix entry),
  `tests/integration/`.
- **Approach:** Use the macOS CI runner to invoke oc-rsync locally (both
  roles on the same machine via `--daemon` mode or local transfer). Stamp
  POSIX ACLs and macOS extended ACLs, transfer, and verify.
- **Complexity:** Medium (CI matrix expansion + harness wiring).
- **Test:** The job itself is the test. Gate on
  `OC_RSYNC_METADATA_INTEROP=1` initially, then graduate to always-on.
- **Platform constraint:** Requires macOS CI runner with xattr and ACL
  support (GitHub `macos-latest` suffices).

### Task T6: Windows metadata CI integration (closes G10)

Enable the `metadata` crate's Windows-specific tests in the Windows CI
matrix. Wire ACL and xattr round-trip harness primitives into a
`windows-latest` job.

- **Location:** `.github/workflows/ci.yml` (`windows-acl-xattr` job
  from `docs/design/windows-acl-xattr-ci-matrix.md`).
- **Approach:** Implement the test plan from the CI matrix doc (section
  3): local push/pull with `-aAX`, daemon push/pull, assert DACL and
  ADS round-trip.
- **Complexity:** Medium-High (Windows CI runner provisioning, SID
  setup, NTFS-only assertions).
- **Test:** The CI job exercises
  `crates/metadata/tests/acl_windows_roundtrip.rs` and
  `crates/metadata/tests/xattr_windows_roundtrip.rs`.
- **Platform constraint:** `windows-latest` runner with NTFS.

### Task T7: ADS namespace prefix for non-root receivers (closes G5)

When receiving ADS stream names without a `user.` prefix on Linux,
either auto-prefix with `user.` (matching how macOS names are handled)
or emit a diagnostic warning with the stream name and skip.

- **Location:** `crates/protocol/src/xattr/prefix.rs` (Linux receiver
  branch, lines 140-161).
- **Approach:** For non-root Linux receivers receiving from a Windows
  source, auto-prefix bare ADS names with `user.` rather than silently
  dropping. Add a `--xattr-prefix-strategy` flag if backward
  compatibility concerns arise.
- **Complexity:** Low (5-10 lines in prefix logic + test).
- **Test:** Transfer a file with an ADS named `mystream` from Windows,
  verify it lands as `user.mystream` on the Linux receiver.
- **Platform constraint:** Linux receiver only.

## 5. Priority ordering

Priority is based on user impact - which gaps affect real-world
cross-platform sync workflows most often.

| Priority | Task | Gap | Rationale |
|----------|------|-----|-----------|
| P0 | T5 | G9 | Test coverage gap is the most dangerous - masks all other defects in the macOS <-> Linux path. |
| P0 | T6 | G10 | Same argument for Windows. Without CI, regressions are invisible. |
| P1 | T4 | G4 | Broken DACL inheritance affects enterprise Windows deployments and may cause security drift. |
| P1 | T3 | G3 | Colon collision silently corrupts Finder tag data on Windows - affects every macOS-to-Windows sync with tagged files. |
| P2 | T2 | G2 | Quarantine footgun affects backup workflows but has a trivial manual workaround (`xattr -d`). |
| P2 | T7 | G5 | Silent drop of non-`user.` ADS affects users who create custom streams, but the population is small. |
| P3 | T1 | G1 | Resource forks > 64 MiB are vanishingly rare in modern macOS usage. |

Gaps G6 (default ACLs), G7 (deny/audit ACEs), and G8 (unresolvable
principals) are structural mismatches between the platform metadata
models. They cannot be fixed without inventing new protocol extensions
or accepting lossy semantics. No fix task is proposed - the current
behaviour (drop with warning) matches upstream rsync.

## 6. Platform-specific constraints

### macOS

- **Quarantine xattr (`com.apple.quarantine`):** Gatekeeper interprets
  this attribute on every `exec`. Backup-restore workflows that preserve
  it cause spurious Gatekeeper prompts. The opt-in strip flag (T2)
  addresses this.
- **64 MiB `getxattr` ceiling:** The macOS kernel caps a single
  `getxattr(2)` at 64 MiB for resource forks. The `position` argument
  must be used to read beyond this limit. This is a macOS kernel
  constraint, not a filesystem limit.
- **No default ACLs:** macOS HFS+/APFS lacks POSIX directory default
  ACLs entirely. This is a permanent structural gap (G6).
- **NFSv4-style extended ACLs:** macOS ACLs support deny entries,
  14-bit permission masks, and inheritance flags that have no POSIX.1e
  equivalent.

### Windows

- **DACL inherited ACE semantics:** The `INHERITED_ACE` flag and
  `SE_DACL_AUTO_INHERITED` control bit govern inheritance chain
  integrity. Writing a DACL without respecting these breaks parent-child
  ACL propagation (G4).
- **ADS stream-name separator (`:`):** Win32 reserves `:` as the stream
  separator in file paths. Xattr names containing `:` (common in macOS
  `com.apple.metadata:*` attributes) collide with this (G3).
- **`LookupAccountNameW` resolution:** ACE principals must resolve to a
  local or domain account on the Windows receiver. Cross-domain
  transfers may silently drop ACEs.
- **`SE_SECURITY_NAME` privilege:** SACL access requires this privilege.
  oc-rsync deliberately excludes SACLs (matching upstream).
- **NTFS requirement:** ADS and DACLs are NTFS-only. FAT32/exFAT
  volumes reject both with I/O errors.
- **Protected DACL policy:** When transferring from POSIX (no
  inheritance context), `PROTECTED_DACL_SECURITY_INFORMATION` is correct
  to prevent spurious parent inheritance. When transferring
  Windows-to-Windows, the source's protection state must be honoured.

### Linux

- **Namespace partitioning:** `user.*` is the only namespace available
  to unprivileged users. `trusted.*`, `security.*` require
  `CAP_SYS_ADMIN`. `system.*` is always skipped (upstream policy).
- **Per-attribute size limits:** ext4 inline xattrs cap at ~4 KiB per
  value; xfs allows ~64 KiB. Large resource forks from macOS may exceed
  these limits and produce `ENOSPC`.
- **POSIX.1e ACLs only:** Linux has no native deny ACE, audit ACE, or
  14-bit permission mask. All macOS/Windows ACL richness collapses to
  rwxrwxrwx.

## 7. Cross-references

- XAP-1: direction-matrix spec - `docs/audit/acl-xattr-direction-matrix.md`
- XAP-2: ACL round-trip harness - `tests/integration/acl_roundtrip.rs`
- XAP-3: xattr round-trip harness - `tests/integration/xattr_roundtrip.rs`
- XAP-8: macOS xattr handling audit - `docs/audit/macos-xattr-handling.md`
- XAP-9: direction-matrix synthesis - `docs/audit/xattr-direction-matrix.md`
- XAP-10: user-facing docs - `docs/user/xattr-acl-cross-platform.md`
- WPC-1: ADS handling audit - `docs/audit/windows-ads-handling.md`
- WPC-10: DACL inheritance audit - `docs/audit/windows-dacl-ace-inheritance.md`
- WAS-1..8: Windows NTFS ACL support - `docs/design/windows-ntfs-acl-support.md`
- CI matrix: `docs/design/windows-acl-xattr-ci-matrix.md`
- ACL crates: `exacl` for POSIX ACLs, `windows` 0.62 (microsoft/windows-rs) for Windows ACLs
- Upstream: `target/interop/upstream-src/rsync-3.4.1/lib/sysxattrs.c`,
  `target/interop/upstream-src/rsync-3.4.1/xattrs.c`,
  `target/interop/upstream-src/rsync-3.4.1/acls.c`
