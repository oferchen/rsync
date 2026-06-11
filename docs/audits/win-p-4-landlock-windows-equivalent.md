# WIN-P.4: Landlock Windows equivalent decision

Status as of 2026-06-11. Re-audit of the daemon-worker sandboxing surface on
Windows, fed by the WIN-P.1 inventory (PR #5643) which flagged
`crates/fast_io/src/dir_sandbox/` as Class **E** (module absent on Windows)
and identified that as the structural blocker for any Landlock-equivalent
investigation.

WIN-S.6 (2026-05-31, `docs/design/windows-landlock-equivalent.md`) already
ran the candidate-API comparison and concluded "keep the stub". This audit
revisits that decision in light of:

1. WIN-P.1's classification finding (`dir_sandbox` is Class E, not just a
   no-op stub - the entire path-based confinement primitive is absent).
2. SEC-1.p shipping the Landlock allowlist into the Linux daemon receiver
   path (URV-5.b, URV-5.c.1-.3 series) with `ref_dirs`, `temp_dir`, and
   `partial_dir` all in the ruleset.
3. URV-LDL-1 confirming `rust-landlock` as the project's preferred
   sandboxing primitive (per `feedback_rust_landlock_preferred`).
4. The `landlock` Cargo feature still defaults off (URV-5.c.5 pending), so
   the *Linux* daemon also runs without Landlock unless explicitly built
   with it. That weakens the Windows "we are behind Linux on hardening"
   argument but does not change the underlying API gap.

The decision in this audit reaffirms WIN-S.6 with one addition: a coarse-
grained "Tier 2 sandbox" option is now spelled out as the only credible
path forward if Windows daemon hardening becomes load-bearing, with an
explicit dependency on a Windows-side equivalent of the `dir_sandbox`
parent-dirfd seam.

## 1. Linux Landlock semantic

Landlock is a Linux LSM (5.13+) that lets an unprivileged thread install a
path-based filesystem-access denial ruleset on itself. The semantic is:

- **Ruleset** declares a set of `AccessFs` rights (read, write, create,
  delete, rename, symlink, refer, truncate) against a set of path roots.
- **`landlock_create_ruleset`** allocates a ruleset fd; rules are added with
  `landlock_add_rule(PathBeneath { allowed_access, parent_fd })`.
- **`landlock_restrict_self`** seals the ruleset onto the calling thread.
  Every subsequent filesystem syscall on that thread, and every process
  inherited from it, is filtered against the union of all rulesets sealed
  onto the thread.
- **Per-thread, irreversible, additive.** A second `restrict_self`
  intersects with the first; rights can only narrow, never widen. No
  `CAP_SYS_ADMIN` required.

ABI versions:

- **v1 (Linux 5.13+)**: read / write / create / delete on regular files,
  symlinks; rename within parent.
- **v2 (Linux 5.19+)**: adds `REFER` (rename / link across allowed
  subtrees).
- **v3 (Linux 6.2+)**: adds `TRUNCATE`.

The project targets v3 with `BestEffort` downgrade through `rust-landlock`
(see `crates/fast_io/src/landlock.rs`).

## 2. Where Landlock is used in oc-rsync

The Linux daemon receiver path engages Landlock once per connection, after
authentication and privilege drop, against:

- `module.path` (always).
- `ref_dirs` (alt-basis: `--copy-dest`, `--link-dest`, `--compare-dest`).
  Wired URV-5.b.1.c (PR #5541).
- `temp_dir` and `partial_dir` if configured (SEC-1.p ruleset).

Call sites:

- `crates/fast_io/src/landlock.rs:80-220` - `restrict_to_module_paths`
  builds the ruleset against `AccessFs::from_all(ABI::V3)` with `BestEffort`
  downgrade and seals it via `restrict_self`.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:292-340` -
  `engage_landlock_sandbox` is the per-connection entry, called from
  `transfer.rs:892`.
- `crates/fast_io/src/dir_sandbox/` (`#![cfg(unix)]`) - the *complementary*
  `*at`-syscall carrier that the daemon uses for explicit path resolution.
  Landlock is the kernel safety net; `dir_sandbox` is the application-level
  primary defense.

Outcome surface: `LandlockOutcome::Enforced(RulesetStatus)`,
`::Unavailable`, `::Error`. Non-Linux always returns `::Unavailable` via
`crates/fast_io/src/landlock_stub.rs` (Class C from WIN-P.1).

## 3. Windows candidate APIs

| Candidate | Granularity | Persistence | Daemon-worker fit | Win32 entry point | Cost |
|---|---|---|---|---|---|
| **Restricted Token** | Per-token DACL-mediated; cannot deny paths it has access to via DACL, only narrow groups/privileges. | Reversible (`RevertToSelf`). | Per-thread via `SetThreadToken`. | `CreateRestrictedToken` + `SetThreadToken`. | 2-3 days privilege stripping; weeks for DACL-based path confinement. |
| **AppContainer** | Per-capability, per-path ACL grant. Default-deny on filesystem. | Per-process, irreversible within the process. | Per-process only. Daemon worker would have to be a child process, not a thread. | `CreateProcessAsUserW` + `STARTUPINFOEX::lpAttributeList` with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`. | 2-3 weeks - fundamental architecture change. |
| **Job Object** | Resource caps + UI restrictions. No filesystem path scope. | Per-process membership; `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` for lifecycle. | Per-process. | `CreateJobObjectW` + `SetInformationJobObject` + `AssignProcessToJobObject`. | 1 day for active-process and memory caps. |
| **Mandatory Integrity Control (Low IL)** | Per-object IL label, write-down prevention. | Per-token. | Per-thread via `SetTokenInformation(TokenIntegrityLevel)`. | `SetTokenInformation`. | 1-2 days to lower the thread token; relies on existing object IL labelling, which oc-rsync does not manage. |
| **WDAC / Code Integrity Policy** | Kernel-enforced code-allowlist. | Per-system policy. | None - blocks code, not filesystem reads/writes. | `SetSystemPolicy`. | Out of scope. |
| **Windows Sandbox / Silos** | Container-level isolation. | Per-process tree. | Server-edition only; heavyweight. | `CreateProcess` + silo APIs. | 2-3 weeks; ops complexity. |

Key structural points (carrying WIN-S.6's analysis):

- **None of these is a path-based allowlist primitive.** Landlock's
  per-subtree `AccessFs` mask has no Windows analogue. Restricted Tokens
  operate inside the existing DACL model; they constrain *which SIDs the
  thread presents* during access checks, but the access decision still
  flows through the DACL on each file. To synthesise Landlock-like
  confinement, every file outside the module root would need a DACL deny
  for the restricted SID, and every file inside would need a grant - the
  daemon cannot set those DACLs on arbitrary system files.
- **AppContainer is the closest semantic match.** Default-deny filesystem
  access + per-capability grants is the Landlock shape. But AppContainer
  is per-process and brings a process-model mismatch (no network by
  default, restricted COM/RPC, registry virtualisation) that requires
  re-architecting the daemon worker to be a child process rather than a
  thread.
- **The `dir_sandbox` seam is the structural dependency.** Without a
  Windows equivalent of the parent-dirfd carrier, even a perfect Landlock
  port would not deliver the symlink-resistance that Linux gets, because
  the *primary* defense (path resolution through `openat2`) does not exist
  on Windows. SEC-1.l audited NTFS handle-based APIs as the structural
  symlink-resistance answer on Windows; that work is the prerequisite for
  any sandbox layer above it.

## 4. Verdict

Three options were considered. Decision: **option (b) "document permanent
gap"** for the WIN-P.4 task itself, with a side note that option (a) "ship
coarse-equivalent" remains available as future hardening if Windows daemon
adoption grows. Option (c) "defer" is rejected because the WIN-P.6
decision matrix needs a closed answer.

### Option (a) - Ship coarse-equivalent ("Tier 2 sandbox")

**What it would look like:** AppContainer + Job Object + Restricted Token
combo on the daemon worker.

- **Restricted Token** strips `SeBackupPrivilege`, `SeRestorePrivilege`,
  `SeTakeOwnershipPrivilege`, `SeDebugPrivilege`. Disables all group SIDs
  except the module owner. Applied per-thread via `SetThreadToken`. Maps
  to the Unix `setuid`/`setgid` + `setgroups([gid])` sequence the daemon
  already runs in `privilege.rs`.
- **Job Object** with `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 4` caps child
  processes from `pre-xfer-exec` / `post-xfer-exec` hooks. Optional memory
  cap. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ensures hook children die
  with the worker.
- **AppContainer** (optional, heaviest): per-connection container SID,
  with explicit per-path capability grants on `module.path`, `ref_dirs`,
  `temp_dir`, `partial_dir`.

**Documented as "Tier 2 sandbox":** weaker than Landlock because it does
not provide path-based syscall denial, but stronger than the current
no-op stub because it strips privileges and caps resource usage.

**Cost:** restricted token + job object alone is 3-4 days. AppContainer
adds 2-3 weeks for the process-spawn architecture change.

**Blockers:** AppContainer requires the daemon worker to become a child
process. Restricted Token alone delivers privilege stripping but no path
scope. Neither closes the symlink-TOCTOU surface that `dir_sandbox`
addresses on Linux.

### Option (b) - Document permanent gap (selected)

**Rationale:**

1. **No Win32 API provides path-based filesystem-access denial** the way
   Landlock does. Every candidate either operates inside the DACL model
   (Restricted Token), requires invasive object labelling (MIC), or
   imposes a process-model change (AppContainer). The WIN-S.6 comparison
   matrix stands.
2. **The structural dependency (`dir_sandbox` Windows equivalent) is
   itself a permanent gap** until SEC-1.l's 20-item NTFS handle-based gap
   list is closed. WIN-P.1 classified `dir_sandbox` as Class E (module
   absent). Until that primitive exists, any sandbox layer above it is
   incomplete.
3. **The Linux daemon also runs without Landlock by default.** URV-5.c.5
   is pending (`landlock` feature flip to default-on); Linux daemon
   operators today rely on the SEC-1 `*at` chain and the LSM-CAP series
   for hardening. The Windows posture being "no sandbox layer, rely on
   NTFS ACLs + Windows Firewall + Group Policy" is symmetric, not a
   regression.
4. **Upstream rsync has no Windows daemon sandbox either.** Matching
   upstream posture is acceptable per project upstream-fidelity
   principle.
5. **SEC-1.l established that file-data writes are structurally safe** on
   Windows via NTFS HANDLEs. The remaining TOCTOU exposure is in
   namespace and metadata operations, which the WPC-V series is migrating
   to handle-based equivalents. Sandboxing is defense-in-depth against
   regressions in that migration, not the primary defense.

**Operator guidance:** Windows daemon operators rely on:

- NTFS DACLs configured by the operator on `module.path` and any
  `ref_dirs` / `temp_dir` / `partial_dir`.
- The daemon service account's group membership (minimised via
  Group Policy or directly via the service installer).
- Windows Firewall and per-rule IP allowlists.
- Active Directory permissions if domain-joined.
- The SEC-1.l handle-based migration as it lands per WPC gap-list item.

This guidance already exists in the SEC-1.l audit text (referenced from
SECURITY.md). No additional documentation needed for this audit beyond
the WIN-P.6 decision matrix entry.

### Option (c) - Defer (rejected)

WIN-P.6 needs a closed verdict to ship the decision matrix. Deferring
WIN-P.4 leaves the matrix incomplete. The audit was scoped to either
close with "ship-equivalent" or close with "permanent-gap"; the
intermediate "defer" pushes the decision into a future audit cycle
without adding new evidence.

## 5. Feed-forward

### WIN-P.6 decision matrix entry

Add row:

| Stub / capability | Linux primitive | Windows decision | Tracking |
|---|---|---|---|
| Per-thread filesystem-path-access denial | Landlock LSM (`landlock_restrict_self`) | **Permanent gap.** No Win32 equivalent. Tier 2 sandbox combo (Restricted Token + Job Object + AppContainer) available as future hardening if Windows daemon adoption grows. Requires `dir_sandbox` Windows equivalent as prerequisite. | WIN-P.4 (this audit), WIN-S.6 (prior audit), SEC-1.l (NTFS handle audit), WPC-V series. |

### WIN-P.9 closure

Close WIN-P.9 ("Implement Landlock Windows equivalent") as **No
implementation - permanent gap.** Cross-reference this audit and WIN-S.6.
Re-open only if a future Windows daemon hardening initiative explicitly
requests the Tier 2 sandbox combo.

### Future Windows daemon hardening (if demand emerges)

Sequenced as:

1. **Prerequisite: Windows `dir_sandbox` equivalent.** Handle-based path
   validation primitive matching the `openat2(RESOLVE_BENEATH)` semantic.
   Tracked under WPC-V verification work. Until this lands, sandbox layers
   above it deliver only partial protection.
2. **Tier 2 sandbox: Restricted Token privilege stripping.** Lowest cost,
   highest value of the candidates. Strips dangerous privileges and
   disables non-essential group SIDs on the worker thread token. Lives in
   `crates/platform/src/privilege.rs` alongside `drop_privileges_windows`.
3. **Tier 2 sandbox: Job Object with active-process + kill-on-job-close.**
   Caps fork-bomb risk from `pre-xfer-exec` / `post-xfer-exec` hooks.
4. **Tier 2 sandbox: AppContainer (heavy).** Only if the project decides
   to make the Windows daemon worker a child process. Carries
   architectural cost; not recommended without a clear demand signal.

DACL-based per-connection confinement is **not** recommended at any tier:
it requires modifying file DACLs at runtime, is racy under concurrent
connections to the same module, and leaves stale ACEs on crash.

## 6. Cross-reference

- **WIN-S.6** (2026-05-31, `docs/design/windows-landlock-equivalent.md`):
  prior decision-of-record. This audit reaffirms it with the WIN-P.1
  classification update and the Tier 2 sandbox option spelled out
  explicitly.
- **WIN-P.1** (2026-06-11, `docs/audits/win-p-1-fast-io-stubs.md`):
  classified `landlock_stub` as Class C and `dir_sandbox` as Class E.
  Identified the `dir_sandbox` seam as a WIN-P.4 prerequisite.
- **SEC-1.l** (2026-05-21, `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`):
  established that file-data writes are structurally safe on Windows via
  NTFS HANDLEs; the 20-item gap list covers remaining namespace and
  metadata path-based syscalls. The SEC-1.l audit is the source of truth
  for the Windows daemon posture documented in this audit.
- **SEC-1.p** (2026-05-22, `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`):
  Linux Landlock allowlist design, kernel-version matrix, and the
  daemon integration plan.
- **URV-5.b.1** (PR #5541): wired `ref_dirs` (alt-basis) into the
  Landlock allowlist. Confirms the Linux ruleset content this audit's
  Windows comparison is measured against.
- **URV-5.c series**: `landlock` Cargo feature default-on plan
  (pending). Notes that even Linux runs without Landlock today unless
  explicitly opted in.
- **URV-LDL-1**: documented `rust-landlock` as the preferred sandboxing
  primitive in the coding guide.
- **WPC-V series**: production-code verification of WPC-3/4/8/9 NTFS
  handle-based path-validation primitives. The structural prerequisite
  for any future Windows sandbox seam.
- **LSM-CAP series**: Linux capability drop hardening. The complementary
  privilege-strip layer that the Windows Restricted Token option would
  mirror.

## 7. Tracking

- Parent: **WIN-P** (#3681).
- This audit: **WIN-P.4** (#3685).
- Prior audit: **WIN-S.6** (closed 2026-05-31).
- Prerequisite for any future implementation: Windows `dir_sandbox`
  equivalent (tracked under WPC-V verification).
- Feed-forward: **WIN-P.6** decision matrix (#3687), **WIN-P.9**
  implementation closure (#3690).
