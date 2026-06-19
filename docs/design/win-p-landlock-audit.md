# WIN-P.4 - Windows Landlock-equivalent candidate audit

**Date:** 2026-06-19
**Status:** AUDIT - feeds **WIN-P.6** decision matrix and **WIN-P.9** closure
**Scope:** Per-candidate audit of Windows process-confinement mechanisms
against the Linux `landlock` semantic that the daemon receiver engages
post-auth. Companion to the longer
`docs/design/win-p-4-landlock-equivalent.md` decision write-up; this doc
is intentionally a single-page audit of the three candidate APIs against
the URV-5.c allowlist that production daemons would need to mirror.

## 1. Linux baseline being matched

`crates/fast_io/src/landlock.rs::restrict_to_module_paths` engages a
path-rooted allowlist on the calling thread. The daemon caller at
`crates/daemon/src/daemon/sections/module_access/transfer.rs:316-318`
composes the allowlist as:

- `module.path` (always - the writable receiver root).
- Each `extra_allowed_paths` entry, which carries the alt-basis chain
  (`--copy-dest` / `--link-dest` / `--compare-dest`) and the
  relocation chain (`--temp-dir` / `--partial-dir` / `--backup-dir`)
  that `validate_client_paths_in_module` (URV-5.b.1) already proved
  resolve beneath `module.path`.

Five load-bearing properties: per-thread scope, irreversible seal,
additive narrowing, path-based denial at the LSM hook, and
runtime-decided roots (no pre-spawn ACL setup required). URV-5.c flips
the feature to default-on for Linux daemons; WIN-P.4 asks whether
Windows can mirror any of this.

## 2. Candidate audit

### 2.1 Restricted Token (`CreateRestrictedToken`)

- **API:** `CreateRestrictedToken` + `SetThreadToken` (MSDN
  [Restricted Tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens),
  [`CreateRestrictedToken`](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken),
  [`SetThreadToken`](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadtoken)).
- **Granularity:** token-level. Three axes: deny-only SIDs
  (`SE_GROUP_USE_FOR_DENY_ONLY`), privilege deletion
  (`SeBackupPrivilege`, `SeTakeOwnershipPrivilege`,
  `SeDebugPrivilege`), and restricting SID list.
- **Path scope:** none native. Confining the worker to `module.path`
  plus alt-basis paths would require runtime DACL rewrites on every
  out-of-scope filesystem object - racy, leaves stale ACEs on crash,
  and is infeasible against system-owned trees.
- **Reversibility:** `RevertToSelf` undoes the token swap - opposite
  of Landlock's irreversible seal.
- **Verdict:** light, per-thread, fits privilege stripping. Cannot
  express the URV-5.c allowlist.

### 2.2 AppContainer (`CreateAppContainerProfile`)

- **API:** `CreateAppContainerProfile` +
  `InitializeProcThreadAttributeList` +
  `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` +
  `CreateProcessAsUserW` (MSDN
  [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation),
  [`CreateAppContainerProfile`](https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-createappcontainerprofile),
  [`UpdateProcThreadAttribute`](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)).
- **Granularity:** per-capability, per-path ACE grants. Default-deny
  on user-profile and filesystem.
- **Path scope:** yes - but only via per-path ACEs pre-applied to each
  target. The URV-5.c allowlist is **runtime-decided** (alt-basis
  paths arrive in the client's argv); mirroring that semantic in
  AppContainer would require the daemon to rewrite ACEs on every
  client-supplied path under load, then revert on disconnect. That is
  operationally heavy and races concurrent connections.
- **Activation:** per-process at spawn time. The receiver worker is
  currently a thread; moving to a child process is a multi-week
  re-architecture and forces a new IPC seam.
- **Symlink semantics:** capability ACE evaluation follows NTFS
  reparse points. No native `RESOLVE_BENEATH` analogue. The
  Linux-side complementary defense (`fast_io::dir_sandbox`) is itself
  `#![cfg(unix)]` (WIN-P.1 Class E), so AppContainer would sit above
  an absent primitive.
- **Verdict:** closest semantic match for path scope, structurally
  incompatible with runtime-decided roots and the current process
  model.

### 2.3 Job Object (`AssignProcessToJobObject`)

- **API:** `CreateJobObjectW` + `SetInformationJobObject` +
  `AssignProcessToJobObject` (MSDN
  [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects),
  [`CreateJobObjectW`](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-createjobobjectw),
  [`AssignProcessToJobObject`](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject)).
- **Granularity:** process-tree resource caps (CPU, memory, active
  process count, UI restrictions) plus
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` lifecycle containment.
- **Path scope:** **none.** Job Objects have no filesystem layer.
- **Verdict:** not a Landlock analogue. Useful only as orthogonal
  hardening for `pre-xfer-exec` / `post-xfer-exec` hook children
  (cap with `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`).

## 3. Tier 2 cross-check

`docs/design/windows-tier2-status.md` (WIN-TIER2.5) marks Windows as
**Tier 2**: CI matrix runs against `x86_64-pc-windows-gnu` only, real
hardware validation is gated behind WIN-G.a/.b/.e/.f. Linux is the
primary daemon platform; Windows daemon adoption is observed only
through bug reports, not benchmarks. Treating the Landlock gap as
**permanent and documented** is symmetric with the upstream rsync
posture (upstream has no Windows daemon sandbox either) and matches
the Tier 2 contract - we do not promise feature parity with Linux,
only correctness within documented capability.

## 4. Recommendation

- **WIN-P.6 row:** record `verdict = PERMANENT GAP (with Tier-2
  mitigation)` per the longer
  `docs/design/win-p-4-landlock-equivalent.md` write-up.
- **WIN-P.9:** close with **no implementation**. Mirror upstream
  rsync's "rely on NTFS DACLs + Windows Firewall + service-account
  minimisation" posture.
- **Future revisit trigger:** Microsoft's
  [Win32 App Isolation](https://github.com/microsoft/win32-app-isolation)
  preview (Windows 11 24H2+) layers AppContainer with a friendlier
  capability model. If it stabilises and gains a runtime-decided
  path-grant API, re-open WIN-P.4 then. Until then, no work.
- **Optional Tier 2 hardening (not scheduled):** Restricted Token for
  privilege stripping (`SeBackupPrivilege`, `SeTakeOwnershipPrivilege`,
  `SeDebugPrivilege`) + Job Object for hook-child containment. Cost
  estimate: 4-5 days combined. Gate on explicit Windows daemon
  adoption signal.

## 5. Risk note: symlink traversal

Linux landlock allows path-bind walks across allowed subtrees via
`LANDLOCK_RULESET_ATTR` v2 `REFER`. AppContainer's ACE evaluation
follows NTFS reparse points but has no equivalent for inter-subtree
moves under a single grant - junctions and mount-point reparse points
either grant full access or deny it. Operators porting Linux-allowlist
configurations would see `--link-dest` flows break when alt-basis
trees live behind junctions.

## 6. References

- `docs/design/win-p-4-landlock-equivalent.md` - longer per-mechanism
  decision write-up.
- `docs/audits/win-p-4-landlock-windows-equivalent.md` - candidate-API
  decision matrix.
- `docs/design/windows-landlock-equivalent.md` - WIN-S.6 first-pass
  evaluation.
- `docs/design/win-p-6-windows-stub-decision-matrix.md` - WIN-P.6
  matrix this audit feeds.
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` -
  Linux allowlist composition.
- `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` -
  NTFS handle-based primitive audit; structural prerequisite for any
  Windows sandbox layer.
