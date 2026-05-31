# WIN-S.6 - Windows Landlock equivalent via restricted tokens

**Date:** 2026-05-31
**Scope:** Evaluate whether the daemon sandbox can use Windows restricted tokens and job objects as a Landlock equivalent, or whether the current stub (no sandbox) is acceptable.
**Status:** DECIDED - stub acceptable for now; restricted tokens recommended as a future hardening layer.
**Inputs:**
- `crates/fast_io/src/landlock.rs` - Linux Landlock implementation (SEC-1.p).
- `crates/fast_io/src/landlock_stub.rs` - Non-Linux stub returning `Unavailable`.
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` - Landlock design and kernel-version matrix.
- `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` - Windows NTFS handle-based dispatch audit.
- `crates/platform/src/privilege.rs` - chroot and privilege drop (no-op stubs on Windows).
- `crates/daemon/src/daemon/sections/module_access/transfer.rs` - `engage_landlock_sandbox` integration point.

## 1. Current state

### 1.1 Linux Landlock implementation

The daemon engages a Landlock ruleset per connection in `engage_landlock_sandbox` (transfer.rs:140-220). After authentication, module path validation, and privilege drop, the function calls `restrict_to_module_paths(&[module.path])`. This applies a kernel-enforced allowlist so every filesystem syscall on the calling thread - and any child process it spawns - is restricted to the module root. The implementation targets ABI v3 (Linux 6.2+) with best-effort downgrade to v1 (Linux 5.13+).

Key properties:
- **Per-thread, irreversible.** Once `restrict_self()` succeeds, the sandbox cannot be relaxed.
- **Defense-in-depth.** The SEC-1 `*at` helpers remain the primary defense; Landlock catches regressions where future code bypasses the dirfd carrier.
- **Additive.** A second `restrict_self()` intersects with the first; it can only narrow, never widen.
- **Unprivileged.** No `CAP_SYS_ADMIN` or root required.

### 1.2 Non-Linux stub

`landlock_stub.rs` mirrors the public API and always returns `LandlockOutcome::Unavailable`. Windows and macOS builds compile against this stub. The daemon logs "landlock unavailable" and continues with SEC-1 `*at` helpers as the sole defense.

### 1.3 Windows daemon posture

The SEC-1.l audit established:
- `use chroot` silently no-ops on Windows (privilege.rs:62-66).
- `drop_privileges` no-ops on Windows; `drop_privileges_windows` does user impersonation via `LogonUserW` + `ImpersonateLoggedOnUser` but does not constrain filesystem scope.
- File-data writes are structurally safe via NTFS HANDLEs.
- Namespace and metadata operations remain path-based and TOCTOU-attackable (20 gap items catalogued in the audit).
- No module-confinement mechanism exists on Windows today.

## 2. Windows sandboxing primitives

### 2.1 Restricted tokens (`CreateRestrictedToken`)

`CreateRestrictedToken` produces a new access token with reduced privileges. Three reduction axes:

| Mechanism | Effect |
|-----------|--------|
| **Disable SIDs** | Marks specified group SIDs as `SE_GROUP_USE_FOR_DENY_ONLY`. ACL checks treat the SID as deny-only - it can deny access but cannot grant it. |
| **Delete privileges** | Removes specified privileges (e.g., `SeBackupPrivilege`, `SeTakeOwnershipPrivilege`) from the token. |
| **Restrict SIDs** | Adds restricting SIDs. When restricting SIDs are present, access checks require *both* the normal DACL check *and* a second check against only the restricting SIDs to pass. |

Usage pattern for daemon sandboxing:
1. Call `CreateRestrictedToken` with the daemon's current token, disabling all group SIDs except the module directory's owner SID and deleting unnecessary privileges.
2. Call `ImpersonateSelf(SecurityImpersonation)` or `SetThreadToken` to apply the restricted token to the current thread.
3. All subsequent file operations on that thread are subject to the restricted token's reduced access.

Relevant APIs (all in the `windows` crate via `windows::Win32::Security`):
- `CreateRestrictedToken` - creates the restricted token.
- `SetThreadToken` - applies a token to the calling thread (analogous to Landlock's per-thread `restrict_self`).
- `RevertToSelf` - reverts to the process token (no Landlock analogue - restricted tokens are reversible).

### 2.2 Job objects (`CreateJobObjectW` + `SetInformationJobObject`)

Job objects provide process-level resource and security constraints. Relevant limit classes:

| Limit | API constant | Effect |
|-------|-------------|--------|
| **UI restrictions** | `JOB_OBJECT_UILIMIT_*` | Restricts clipboard, desktop, display, global atoms, handles, read clipboard, system parameters, exit Windows. Not filesystem-related. |
| **Process limit** | `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` | Caps the number of processes in the job. Useful for preventing fork bombs from pre/post-xfer-exec hooks. |
| **Network** | `JOB_OBJECT_NET_RATE_CONTROL_*` | Network bandwidth limiting (Windows 10+). Not directly relevant. |

Job objects do not provide filesystem path restrictions. There is no `JOB_OBJECT_LIMIT_FILESYSTEM_ACCESS` or equivalent. Job objects constrain resource usage (CPU, memory, process count, network), not filesystem scope.

### 2.3 Integrity levels (mandatory integrity control)

Windows Vista+ assigns an integrity level (IL) to every process token and every securable object. A process at a lower IL cannot write to objects at a higher IL, regardless of DACL permissions. Levels: Untrusted (0), Low (0x1000), Medium (0x2000), High (0x3000), System (0x4000).

For daemon sandboxing:
- Set the daemon worker thread to Low IL via `SetTokenInformation(TokenIntegrityLevel)`.
- Mark the module directory and its contents as Low IL.
- Files outside the module root remain at Medium or higher IL, so the worker thread cannot write to them.

Drawback: integrity levels are object-level labels, not path-based allowlists. Labeling every file under the module root as Low IL is invasive and changes the security properties of those files for all processes, not just the daemon.

### 2.4 AppContainer

AppContainers (Windows 8+) provide the closest analogue to Landlock:
- A process runs in an AppContainer with a unique SID.
- By default, an AppContainer process has no access to any filesystem location.
- Explicit capabilities and per-path ACL entries grant access to specific directories.
- AppContainer isolation is enforced by the kernel, similar to Landlock.

However, AppContainers are designed for UWP/sandboxed desktop applications, not for server daemon workers. They impose significant constraints (no network access by default, limited COM/RPC, restricted registry) that would require extensive adaptation for a daemon use case.

### 2.5 Windows Sandbox / Containers (silos)

Windows Server provides process silos (application containers at the kernel level) and Hyper-V containers. These are heavyweight isolation mechanisms unsuitable for per-connection daemon workers.

## 3. Comparison matrix

| Property | Linux Landlock | Windows restricted tokens | Windows job objects | Windows integrity levels | Windows AppContainer |
|----------|---------------|--------------------------|--------------------|--------------------------|--------------------|
| Filesystem path restriction | Yes (per-subtree allowlist) | Indirect (via DACL + restricting SIDs) | No | Indirect (per-object labels) | Yes (per-capability grants) |
| Per-thread | Yes | Yes (SetThreadToken) | No (per-process/job) | Yes (per-token) | No (per-process) |
| Irreversible | Yes | No (RevertToSelf) | Partial (once assigned) | No (elevatable with privilege) | Yes (per-process) |
| Unprivileged | Yes | Yes | Yes | Needs `SeTcbPrivilege` for Low IL on others' objects | Needs `SeTcbPrivilege` |
| Kernel-enforced | Yes | Yes | Yes | Yes | Yes |
| Complexity | Low (crate wraps syscall) | Medium (token manipulation + DACL awareness) | Low but wrong scope | High (per-object labeling) | High (process model mismatch) |
| Granularity | Read/write/create/delete/rename per subtree | Grant/deny per SID per object | Resource limits only | Write-down prevention by level | Per-capability | 

## 4. Analysis

### 4.1 Why restricted tokens are not a direct Landlock equivalent

Landlock provides a path-based allowlist: "this thread can only access files under `/srv/module-a`". The restriction is *independent* of the filesystem's permission model.

Windows restricted tokens operate *within* the existing DACL model. They constrain which SIDs the thread presents during access checks, but the actual access decision depends on the DACL on each file. To achieve Landlock-like confinement via restricted tokens, you would need to:

1. Ensure every file *outside* the module root has a DACL that denies the restricted SID.
2. Ensure every file *inside* the module root has a DACL that grants the restricted SID.

This is effectively "configure the filesystem permissions correctly" - which is already the operator's responsibility and has nothing to do with a per-connection sandbox. The daemon cannot modify DACLs on arbitrary system files, so it cannot create a Landlock-like "deny everything outside my root" posture via restricted tokens alone.

### 4.2 What restricted tokens can do

Even without Landlock-equivalent path confinement, restricted tokens provide meaningful hardening:

1. **Privilege stripping.** Remove `SeBackupPrivilege`, `SeRestorePrivilege`, `SeTakeOwnershipPrivilege`, `SeDebugPrivilege`, and other dangerous privileges. This limits the blast radius of a compromised daemon worker - it cannot bypass DACLs, attach to other processes, or take ownership of system files.
2. **Group removal.** Disable membership in `Administrators`, `Backup Operators`, and other privileged groups so the worker's effective access matches its stated user, not the daemon service account's full group membership.
3. **Deny-only SIDs.** Mark the worker's non-essential SIDs as deny-only so they cannot grant access to resources.

These are defense-in-depth measures analogous to the Unix `setuid`/`setgid` drop already implemented in `privilege.rs`, but more fine-grained.

### 4.3 Job objects as complementary control

Job objects cannot restrict filesystem access, but they can:
- Limit the number of child processes (prevents fork-bomb via pre/post-xfer-exec hooks).
- Enforce memory limits per connection.
- Prevent the worker from creating new desktops or accessing the clipboard (relevant if running as a Windows service with desktop interaction).

These are resource-containment controls, not access-control controls. They complement restricted tokens but do not replace Landlock.

## 5. Recommendation

### 5.1 The stub is acceptable today

The current `landlock_stub.rs` returning `Unavailable` on Windows is the correct posture for these reasons:

1. **No Windows API provides Landlock-equivalent filesystem path confinement** without invasive DACL modifications. Restricted tokens constrain the token's effective access but depend on the existing DACL model. Integrity levels require per-object labeling. AppContainers impose a process model mismatch. None of these provide a simple "restrict this thread to paths under X" primitive.

2. **The SEC-1.l audit established that file-data writes are structurally safe** on Windows via NTFS HANDLEs. The remaining TOCTOU exposure is in namespace/metadata operations, which are being migrated to handle-based equivalents (the 20-item gap list). Sandboxing is defense-in-depth against regressions in that migration, not the primary defense.

3. **Windows daemon deployment is a secondary target.** Upstream rsync does not support Windows daemon mode natively. oc-rsync's Windows daemon support is already ahead of upstream in this regard. The threat model for a Windows daemon operator is different: they typically run behind Windows Firewall, NTFS permissions, and Group Policy, which collectively provide module-level confinement that Landlock adds on Linux.

4. **Upstream rsync does not sandbox on Windows either.** No chroot, no Landlock, no restricted tokens. Matching upstream's posture is acceptable per the project's upstream-fidelity principle.

### 5.2 Future work: restricted token hardening (optional, not blocking)

If Windows daemon hardening becomes a priority, the recommended approach is:

1. **Restricted token for privilege stripping** (medium effort, high value):
   - After `drop_privileges_windows`, call `CreateRestrictedToken` to strip `SeBackupPrivilege`, `SeRestorePrivilege`, `SeTakeOwnershipPrivilege`, `SeDebugPrivilege`.
   - Disable all group SIDs except the module owner's SID.
   - Apply via `SetThreadToken` on the connection-handling thread.
   - Implementation crate: `windows` (already a dependency for IOCP, ACL, service integration).
   - This mirrors the Unix `setuid`/`setgid` + `setgroups([gid])` sequence.

2. **Job object for process limits** (low effort, moderate value):
   - Wrap each daemon connection's worker thread in a job object with `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 4` to cap child processes from hooks.
   - Optionally set memory limits.

3. **DACL-based confinement** (high effort, high value, invasive):
   - Create a per-connection restricted SID.
   - On connection setup, add a grant ACE for the restricted SID to the module root (recursively or lazily).
   - Apply a restricting SID list to the token so the DACL must grant both the normal SID and the restricted SID.
   - On connection teardown, remove the grant ACE.
   - This is invasive (modifies file DACLs), racy (concurrent connections to the same module), and fragile (crash leaves stale ACEs). Not recommended.

4. **AppContainer exploration** (high effort, uncertain value):
   - Investigate whether a per-connection AppContainer process is viable.
   - Would require spawning the transfer as a child process in an AppContainer rather than running it on a thread.
   - Fundamental architecture change; not recommended without a clear demand signal.

### 5.3 Decision

- **Keep the stub.** `landlock_stub.rs` returning `Unavailable` on Windows is correct and sufficient.
- **Document the Windows daemon posture.** The SEC-1.l audit's SECURITY.md text already covers this. No additional documentation needed.
- **Track restricted-token privilege stripping** as a future hardening item if Windows daemon adoption grows. This is independent of the Landlock stub and would live in `crates/platform/src/privilege.rs` alongside the existing `drop_privileges_windows`.
- **No code changes required** for this audit item.

## 6. Implementation cost estimates

| Item | Effort | Value | Priority |
|------|--------|-------|----------|
| Keep stub (this decision) | None | Correct posture documented | Done |
| Restricted token privilege stripping | 2-3 days | Reduces blast radius of compromised worker | Low - no demand signal |
| Job object process limits | 1 day | Prevents hook fork bombs | Low - niche scenario |
| DACL-based confinement | 1-2 weeks | Landlock-like path restriction | Not recommended - invasive |
| AppContainer per-connection | 2-3 weeks | Strong isolation | Not recommended - architecture change |
