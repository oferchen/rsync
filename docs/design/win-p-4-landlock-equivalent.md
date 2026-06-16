# WIN-P.4 - Windows path sandboxing audit (Landlock equivalent)

**Date:** 2026-06-16
**Status:** DECIDED - **PERMANENT GAP** with Tier-2 mitigation path
**Scope:** Evaluate three Windows process-confinement mechanisms -
Restricted Tokens (`CreateRestrictedToken`), AppContainer
(`CreateAppContainerProfile` + `CreateProcessAsUserW`), Job Objects
(`AssignProcessToJobObject`) - as Windows analogues to Linux Landlock
for the daemon-worker filesystem sandbox. Feeds WIN-P.6
(`docs/design/win-p-6-windows-stub-decision-matrix.md`) with a per-stub
verdict for `landlock_stub`.

Companion audit: `docs/audits/win-p-4-landlock-windows-equivalent.md`
(WIN-P.4 candidate-API decision matrix). Prior design doc:
`docs/design/windows-landlock-equivalent.md` (WIN-S.6 first-pass
evaluation). Prior structural audit:
`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` (SEC-1.l,
NTFS handle-based dispatch).

## 1. Linux semantics being matched

Landlock (Linux 5.13+) lets an unprivileged thread install a path-based
filesystem-access denial ruleset on itself. Operating modes are
declared with `AccessFs::*` masks; rules are added via
`landlock_add_rule(PathBeneath { allowed_access, parent_fd })` and
sealed with `landlock_restrict_self`.

ABI versions in use:

| ABI | Kernel | Adds |
|---|---|---|
| v1 | 5.13+ | `READ_FILE`, `WRITE_FILE`, `READ_DIR`, `REMOVE_DIR`, `REMOVE_FILE`, `MAKE_*`, `RENAME_*` |
| v2 | 5.19+ | `REFER` (rename / link across allowed subtrees) |
| v3 | 6.2+ | `TRUNCATE` |
| v4 | 6.7+ | TCP socket access scopes (not used) |

Project targets v3 with `BestEffort` downgrade (`rust-landlock` crate;
URV-LDL-1 documented preference). Daemon engagement happens
post-auth, post-privilege-drop, against `module.path` plus `ref_dirs`
(alt-basis), `temp_dir`, and `partial_dir` (SEC-1.p ruleset). Call site:
`crates/fast_io/src/landlock.rs:80-220` `restrict_to_module_paths`, driven
from `crates/daemon/src/daemon/sections/module_access/transfer.rs`
`engage_landlock_sandbox`.

Five load-bearing semantic properties to match:

1. **Per-thread** scope (not per-process). The daemon worker is a thread
   serving one connection; the parent listener keeps full privileges.
2. **Irreversible** within the thread. Once sealed, only narrowing is
   allowed; no `RevertToSelf` analogue exists.
3. **Additive intersection** across multiple seals - rights can only
   narrow.
4. **Path-based denial** at the LSM hook layer, evaluated against every
   filesystem syscall.
5. **Symlink rejection** complementing `dir_sandbox`'s `openat2
   RESOLVE_BENEATH` chain (which is the application-level primary
   defense; Landlock is the kernel safety net for regressions).

## 2. Windows candidate mechanisms

### 2.1 Restricted Token (`CreateRestrictedToken`)

A `CreateRestrictedToken` call produces a new access token with reduced
privileges. Three orthogonal reduction axes: disable group SIDs
(`SE_GROUP_USE_FOR_DENY_ONLY`), delete privileges (e.g.
`SeBackupPrivilege`, `SeTakeOwnershipPrivilege`), and add restricting
SIDs (forces a second access check against only the restricting SID
set). The restricted token is then applied to the calling thread via
`SetThreadToken`, with `RevertToSelf` to undo.

| Property | Value |
|---|---|
| Granularity | Token-level. DACL-mediated access checks consult the token's group SIDs and privileges. |
| Path scope | **None native.** Confinement to a specific directory tree requires the filesystem objects outside the tree to deny the restricted SID via their DACLs. The daemon cannot rewrite arbitrary system DACLs at runtime. |
| Persistence across child processes | Inherited via `CreateProcessAsUserW` only if the spawn uses the restricted token explicitly. Threads spawned in the same process inherit unless they call `SetThreadToken`. |
| Runtime activation cost | Microseconds: `CreateRestrictedToken` + `SetThreadToken`. |
| Kernel version floor | Windows 2000+ (all supported releases). |
| Reversibility | **Reversible** via `RevertToSelf` - opposite of Landlock's irreversible seal. |
| Symlink-resistance | None at this layer. NTFS reparse points (mount points, symlinks, junctions) are followed transparently during DACL evaluation. |

MSDN references:
<https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken>,
<https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadtoken>,
<https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens>.

### 2.2 AppContainer (`CreateAppContainerProfile`)

An AppContainer is a capability-based isolation primitive. The process
runs with a generated container SID; default DACLs on the user profile,
registry, and filesystem deny that SID. Access to specific resources
requires explicit per-path ACE grants tied to capability SIDs declared
via `SECURITY_CAPABILITIES`.

The container is established at process spawn:
`InitializeProcThreadAttributeList` + `UpdateProcThreadAttribute` with
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, then
`CreateProcessAsUserW` with the constructed `STARTUPINFOEX`.

| Property | Value |
|---|---|
| Granularity | Per-capability, per-path ACE grants. Default-deny on filesystem. |
| Path scope | **Yes**, via per-path ACE grants tied to capability SIDs. Closest semantic match to Landlock's path allowlist. |
| Persistence across child processes | Inherited; AppContainer is per-process and propagates to child processes spawned from within. |
| Runtime activation cost | High: requires a process spawn. **Cannot be applied to an already-running thread.** |
| Kernel version floor | Windows 8 / Server 2012+. |
| Reversibility | None within the process - irreversible, matching Landlock. |
| Symlink-resistance | Capability ACE evaluation follows reparse points; no native `RESOLVE_BENEATH` analogue. The capability grant is on the canonical path. |

MSDN references:
<https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-createappcontainerprofile>,
<https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation>,
<https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw>,
<https://learn.microsoft.com/en-us/windows/win32/procthread/process-and-thread-extended-attributes>.

### 2.3 Job Object (`AssignProcessToJobObject`)

A Job Object is a kernel namespace grouping one or more processes under
a single set of resource limits, UI restrictions, and a kill-on-close
flag. Created via `CreateJobObjectW`, configured via
`SetInformationJobObject` with `JOBOBJECTINFOCLASS` values (basic
limits, extended limits, UI restrictions, CPU rate control,
network rate control), processes are added via
`AssignProcessToJobObject`.

| Property | Value |
|---|---|
| Granularity | Process-tree resource and UI caps. |
| Path scope | **None.** Job Objects do not have a filesystem-path concept. The closest filesystem-related limit is none; the closest related limit is `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` for lifecycle containment. |
| Persistence across child processes | All processes assigned to the Job inherit limits; child processes are auto-assigned if `JOB_OBJECT_LIMIT_BREAKAWAY_OK` is not set. |
| Runtime activation cost | Low: microseconds for `CreateJobObjectW` + `SetInformationJobObject` + `AssignProcessToJobObject`. |
| Kernel version floor | Windows 2000+; nested jobs require Windows 8+. |
| Reversibility | None - process cannot leave the Job. |
| Symlink-resistance | Not applicable; no path layer. |

MSDN references:
<https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-createjobobjectw>,
<https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject>,
<https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects>,
<https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information>.

## 3. Comparison against Landlock semantics

| Landlock property | Restricted Token | AppContainer | Job Object |
|---|---|---|---|
| **Path-based denial** (`AccessFs` mask vs path roots) | No - operates on token SIDs against existing DACLs | **Closest match** - per-path ACE grants tied to capability SIDs | No - no path concept |
| **Per-thread scope** | Yes (via `SetThreadToken`) | No - per-process at spawn time | No - per-process membership |
| **Irreversible seal** | No (`RevertToSelf` reverts) | Yes within process | Yes |
| **Additive intersection** | Partial - multiple `SetThreadToken` calls replace, do not intersect | No - one container per process | No |
| **Symlink rejection** (`RESOLVE_BENEATH` complement) | None | None - capability grants follow reparse points | Not applicable |
| **Unprivileged engagement** | Yes - no `SeAssignPrimaryTokenPrivilege` needed when narrowing | Mostly - AppContainer profile creation under user scope; `CreateProcessAsUserW` is the integration cost | Yes |

The structural blocker: **none of the three Windows mechanisms map
cleanly to the per-thread, irreversible, path-based denial semantic**
that Landlock provides. AppContainer is the closest semantic match for
path scope, but it is per-process at spawn time, which forces a
process-model architecture change (daemon worker becomes a child
process, not a thread). Restricted Tokens are per-thread but have no
native path scope; synthesising one would require DACL modification on
all out-of-scope filesystem objects, which is racy under concurrent
connections and leaves stale ACEs on crash.

The complementary blocker: `crates/fast_io/src/dir_sandbox/` is
`#![cfg(unix)]`. The parent-dirfd carrier that `openat2
RESOLVE_BENEATH` builds on top of has no Windows equivalent today.
SEC-1.l audited NTFS handle-based APIs (`NtCreateFile` with relative
`RootDirectory`, `FILE_OPEN_REPARSE_POINT`) as the structural
symlink-resistance answer; that 20-item gap list is the prerequisite
for any sandbox layer above it.

## 4. Map to oc-rsync daemon use case

The daemon needs the worker thread for a single connection to be
confined to:

- `module.path` (always - the receiving / sending root).
- `ref_dirs` (alt-basis: `--copy-dest`, `--link-dest`,
  `--compare-dest`) read-only.
- `temp_dir` and `partial_dir` if configured (read-write).
- The daemon's TCP socket fd (transitively, via inherited handles).

| Requirement | Restricted Token fit | AppContainer fit | Job Object fit |
|---|---|---|---|
| Restrict thread to allowed paths | No (DACL workaround infeasible) | Yes (per-path ACE grants) | No |
| Read-only mount on `ref_dirs` | No native; relies on DACL deny | Yes (per-capability grant) | No |
| Strip dangerous privileges (`SeBackupPrivilege`, `SeRestorePrivilege`, `SeTakeOwnershipPrivilege`, `SeDebugPrivilege`) | **Yes** - native fit | Implicit (containers start without these) | No |
| Cap `pre-xfer-exec` / `post-xfer-exec` child process count | No | No | **Yes** (`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`) |
| Kill hook children with worker | No | No | **Yes** (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) |
| Apply per-connection (per-worker-thread) | Yes | No - per-process | No - per-process |
| Survive `dir_sandbox` absence | No - structural prerequisite missing | No - structural prerequisite missing | Not applicable |

The Restricted Token + Job Object combination delivers privilege
stripping and hook-child containment, but it does not deliver
path-based filesystem confinement. AppContainer delivers path
confinement but requires re-architecting the daemon worker to be a
child process - a multi-week change. Neither combination delivers the
symlink-resistance that `dir_sandbox` provides on Linux.

## 5. Verdict

**PERMANENT GAP** for the Landlock-equivalent path-based denial
semantic, with a documented Tier-2 mitigation path. Selected option (b)
from the WIN-P.4 audit
(`docs/audits/win-p-4-landlock-windows-equivalent.md` §4) over option (a).

**Three converging reasons:**

1. **No Win32 API provides path-based filesystem-access denial** the way
   Landlock does. Every candidate is process-level (AppContainer, Job
   Object) or operates inside the DACL model (Restricted Token).
2. **The structural prerequisite (`dir_sandbox` Windows equivalent) is
   itself a permanent gap** until SEC-1.l's 20-item NTFS handle-based
   gap list is closed. WIN-P.1 classified `dir_sandbox` as Class E
   (module absent on Windows). Sandbox layers above an absent
   primary-defense primitive deliver only partial protection.
3. **Upstream rsync has no Windows daemon sandbox either.** Matching
   upstream posture is acceptable per project upstream-fidelity
   principle; the Windows daemon posture being "rely on NTFS ACLs +
   Windows Firewall + Group Policy + service-account minimisation" is
   symmetric with upstream, not a regression.

**Tier-2 mitigation path** (deferred until Windows daemon adoption
generates explicit demand, gated on `dir_sandbox` Windows equivalent
landing first):

- **Restricted Token** for privilege stripping
  (`SeBackupPrivilege`, `SeRestorePrivilege`,
  `SeTakeOwnershipPrivilege`, `SeDebugPrivilege` deleted; non-essential
  group SIDs marked `SE_GROUP_USE_FOR_DENY_ONLY`). Applied per-worker
  thread via `SetThreadToken`. Mirrors the Unix
  `setuid` / `setgid` / `setgroups([gid])` sequence in
  `crates/platform/src/privilege.rs::drop_privileges`. Cost: ~3-4 days.
- **Job Object** with `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` and
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` to contain
  `pre-xfer-exec` / `post-xfer-exec` hook children. Cost: ~1 day.
- **AppContainer**: not recommended without a clear demand signal -
  forces daemon worker to become a child process. Cost: ~2-3 weeks.

DACL-based per-connection confinement is **explicitly not recommended**:
it requires runtime DACL modification on shared filesystem objects, is
racy under concurrent connections, and leaves stale ACEs on crash.

## 6. Feed-forward

- **WIN-P.6 (#3687)** decision matrix: landlock row records
  `verdict = PERMANENT GAP (with Tier-2 mitigation)`, with this design
  doc as the ship-or-gap reference and WIN-P.4 (audits) as the candidate
  matrix.
- **WIN-P.9 (#3690)** "Implement Landlock Windows equivalent" closes
  with **no implementation**. Reference this audit, WIN-P.4 (audits),
  WIN-S.6, and SEC-1.l.
- **Future Windows daemon hardening:** sequencing recorded above; not
  scheduled. Re-open if Windows daemon adoption produces explicit
  hardening demand.
- **WIN-TIER2.5** Tier-2 caveat documents Landlock as a permanent gap
  on Windows with the Tier-2 mitigation path as future work, not a
  regression vs upstream.

## 7. References

- `docs/audits/win-p-4-landlock-windows-equivalent.md` - companion
  candidate-API audit.
- `docs/design/windows-landlock-equivalent.md` (WIN-S.6) - prior
  design write-up.
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` -
  Linux Landlock allowlist and kernel-version matrix.
- `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`
  (SEC-1.l) - NTFS handle-based dispatch audit; structural
  prerequisite for any Windows sandbox layer.
- `docs/design/win-p-6-windows-stub-decision-matrix.md` - WIN-P.6
  matrix that consumes this verdict.
- MSDN `CreateRestrictedToken`:
  <https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken>.
- MSDN `SetThreadToken`:
  <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadtoken>.
- MSDN Restricted Tokens overview:
  <https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens>.
- MSDN `CreateAppContainerProfile`:
  <https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-createappcontainerprofile>.
- MSDN AppContainer isolation:
  <https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation>.
- MSDN `CreateProcessAsUserW`:
  <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw>.
- MSDN `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`:
  <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute>.
- MSDN `CreateJobObjectW`:
  <https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-createjobobjectw>.
- MSDN `AssignProcessToJobObject`:
  <https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject>.
- MSDN Job Objects overview:
  <https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects>.
- Linux Landlock kernel docs:
  <https://docs.kernel.org/userspace-api/landlock.html>.
- `rust-landlock` crate: <https://docs.rs/landlock/>.

## 8. Tracking

- Parent: **WIN-P** (#3681).
- This document: design-side companion to **WIN-P.4** (#3685).
- Closes: WIN-P.4 task with verdict **PERMANENT GAP** (with Tier-2
  mitigation documented).
- Feeds: **WIN-P.6** (#3687), **WIN-P.9** (#3690) no-implementation
  closure.
- Prerequisite for any future implementation: Windows `dir_sandbox`
  equivalent (tracked under WPC-V verification work).
