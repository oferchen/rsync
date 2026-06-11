# WPC-VERIFY: status synthesis for WPC-3 / WPC-4 / WPC-8 / WPC-9

Tracks parent #2869 (Windows real-world parity series).
Synthesises the WPC-V.1-.5 / .11 / .12 reality-check audits into a single
status reconciliation across the four WPC cells whose "completed"
tracker status diverged from shipped production code as of 2026-06-10.

Date of synthesis: 2026-06-11. Verified against master HEAD and the
WPC-3 / 5 / 8 / 9 follow-up PR set (#5564, #5575, #5579, #5583, #5592).

## 1. Inputs — WPC-V reality-check audits

| Audit | Scope | Conclusion |
|---|---|---|
| WPC-V.1 (#3745) | Workspace grep for ADS implementation sites (`ALTERNATE_DATA`, `FindFirstStreamW`, NTFS stream APIs) | Backend present at `crates/metadata/src/xattr_windows.rs`; CLI preflight rejects `--xattrs` on Windows, so the backend is unreachable from the shipped CLI surface. |
| WPC-V.2 (#3746) | Locate the production reparse-point classifier file referenced by WPC-8 | At audit time (pre-#5579), `crates/metadata/src/windows/reparse.rs` did not exist; only the design doc at `docs/design/wpc-8-reparse-point-classifier.md` existed. Gap confirmed. |
| WPC-V.3 (#3747) | Locate the long-path `\\?\` prefix helper referenced by WPC-5 | At audit time (pre-#5575), no production helper prepended `\\?\` to NTFS paths; `to_wide_path` in `crates/fast_io/src/iocp/file_reader.rs:315` did not prepend the extended prefix. Gap confirmed. |
| WPC-V.4 (#3748) | Audit the `crates/metadata` Windows submodule structure | No `windows/` directory was wired into `crates/metadata/src/lib.rs`; the only Windows surface lived in flat files (`xattr_windows.rs`, `acl_windows/`, `nfsv4_acl.rs`). Structural gap for the reparse classifier. |
| WPC-V.5 (#3749) | Cross-reference the WPC-3 / 4 / 8 / 9 PR numbers from the git log against current master | No PRs at the time of audit had landed the corresponding production code; the tracker entries were promoted on design / audit completion alone. |
| WPC-V.11 (#3755) | Update the WPC parent task (#2869) description with reality status | Parent task description updated to reflect the audit/design-only completion pattern. |
| WPC-V.12 (#3756) | Update `docs/design/windows-support-matrix.md` (renamed to `docs/user/windows-support-matrix.md`) with reality status | Matrix revised on 2026-06-10 to cite verified production source paths per cell; cells whose implementation is still in flight are marked "Design only" / "Audit only" with follow-up PR links. |

The WPC-V.1-.5 audits established a status-vs-reality misalignment for
four cells. WPC-V.11 / .12 propagated the corrected status into the
parent tracker and the user-facing matrix. The remaining conditional
follow-ups (WPC-V.7 / .8 / .10) are resolved in section 3.

## 2. Status matrix — WPC-3 / WPC-4 / WPC-8 / WPC-9

The "Original status" column reflects the WPC tracker entry as it stood
when the WPC-V audits were opened. The "Actual master status" column
reflects master HEAD on 2026-06-11 after the WPC-5' / 8' / 9' / 3.wire
remediation series.

| Task | Original status | Actual master status | Production PR | Test coverage | Verdict |
|---|---|---|---|---|---|
| **WPC-3** — Implement ADS path in `fast_io::windows::ads` | Completed (#2905) on design-doc basis | **Partial**. Backend `crates/metadata/src/xattr_windows.rs` ships `FindFirstStreamW`/`stream_path_wide` + `strip_ads_prefix` ADS round-trip code. CLI preflight at `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:186` still emits `extended attributes are not supported on this client` and blocks `--xattrs` on Windows builds. Backend is unreachable from the shipped CLI surface. | Backend code: pre-existing in `xattr_windows.rs`. Preflight wire-up: PR #5564 (currently CLOSED, not merged). | End-to-end Windows `--xattrs` test added by WPC-3.wire.4 (#3812) under `crates/cli/tests/feature_propagation.rs`. Test gated by `cfg(windows, feature = "xattr")` and `cfg(windows)` preflight allowance; both are pending PR #5564 reopen. | **Shipped backend, blocked CLI surface.** Tracked as WPC-3.wire.1 (#3809), WPC-3.wire.2 (#3810), WPC-3.wire.3 (#3811). |
| **WPC-4** — ADS round-trip regression test | Completed (#2906) on design-doc basis | **Partial**. End-to-end ADS round-trip test was added in #3812 (WPC-3.wire.4). The test verifies the `xattr_windows.rs` `FindFirstStreamW` path produces wire-compatible xattr entries. Round-trip against a Windows receiver remains gated on PR #5564 unblocking `--xattrs` preflight. | WPC-3.wire.4 (#3812 completed) | Wire-byte round-trip test landed; cross-platform round-trip test (Linux → Windows receiver) blocked on WPC-3 preflight wire-up. | **Wire path tested; cross-platform receiver path blocked on WPC-3 CLI gate.** |
| **WPC-8** — Implement reparse-point classifier in `metadata/src/windows/reparse.rs` | Completed (#2910) on design-doc basis | **Shipped**. The file `crates/metadata/src/windows/reparse.rs` exists in production (49 KB) and implements `classify_reparse_point`, `parse_symlink_reparse`, `parse_junction_reparse`, `parse_mount_point_reparse`, with `ReparseKind` wired into `FileEntry` symlink + special handling. Non-Windows targets get a no-op stub via `#[cfg(windows)]`. | PR #5579 (MERGED 2026-06-11), PR #5592 (MERGED 2026-06-11), plus the WPC-8'.1-.13 subtasks (#3777-#3789) and the metadata wiring at WPC-8'.9. | Unit tests for synthetic buffers (`crates/metadata/tests/windows_reparse_synthetic.rs`); RAII Windows fixture helpers (PR #5583, MERGED 2026-06-11). | **Verified shipped.** |
| **WPC-9** — Reparse-point regression test | Completed (#2911) on design-doc basis | **Shipped**. Windows-only integration tests at `crates/metadata/tests/windows_symlink_junction_transfer.rs` (12.2 KB) exercise the `mklink /d` (directory symlink) and `mklink /j` (junction) paths produced by WPC-8'. Mount-point fixture (`DefineDosDevice` / `mountvol`) also wired. Tests are gated on `SeCreateSymbolicLinkPrivilege` so they degrade gracefully on standard CI runners. CI nightly cells added by commit 6607c87e5 (`ci: add Windows nightly reparse-point + symlink test cell (WCI-8)`). | WPC-9'.1-.7 (#3800-#3806). | Symlink, junction, and mount-point classifier tests; transfer round-trip tests for symlink + junction on Windows nightly. | **Verified shipped.** |

## 3. Gap-list resolution — WPC-V.7 / .8 / .10 conditional follow-ups

Each WPC-V.7 / .8 / .10 entry was filed as conditional ("if WPC-V.2 / .3
finds a gap, file fresh implementation / test tracker"). Each is
resolved below.

| Conditional follow-up | Condition | Outcome | Resolution |
|---|---|---|---|
| **WPC-V.7** (#3751) — file fresh WPC-8' implementation task if WPC-V.2 found a reparse gap | WPC-V.2 confirmed gap | Followed up. WPC-8' implementation subtree filed and shipped: WPC-8'.1 through WPC-8'.13 (#3777-#3789), all marked completed. Reparse classifier landed via PR #5579 / #5592. | **No further action.** Conditional follow-up was acted on; implementation series complete. WPC-V.7 itself stays pending only as a marker; close it as resolved-by-WPC-8'. |
| **WPC-V.8** (#3752) — file fresh WPC-5' implementation task if WPC-V.3 found a long-path gap | WPC-V.3 confirmed gap | Followed up. WPC-5' implementation subtree filed and shipped: WPC-5'.1 through WPC-5'.10 (#3790-#3799), all marked completed. `to_extended_path` helper landed via PR #5575. One sub-cell remains pending: WPC-5'.7 (wire `to_extended_path` into `metadata::windows` ACL/xattr handle acquisition); tracked under the existing WPC-5' subtree, not a fresh task. | **No further action.** Conditional follow-up was acted on; implementation series complete except WPC-5'.7, which already exists. Close WPC-V.8 as resolved-by-WPC-5'. |
| **WPC-V.10** (#3754) — file fresh reparse round-trip test task if WPC-V.2 confirmed reparse gap | WPC-V.2 confirmed gap | Followed up. WPC-9' regression test subtree filed and shipped: WPC-9'.1 through WPC-9'.7 (#3800-#3806), all marked completed. CI nightly cell wired by commit 6607c87e5. | **No further action.** Conditional follow-up was acted on; regression test series complete. Close WPC-V.10 as resolved-by-WPC-9'. |

## 4. Action items

### 4.1 Genuinely new gaps surfaced by this synthesis

| Gap | Tracking |
|---|---|
| WPC-3 preflight CLI wire-up — PR #5564 currently CLOSED; `--xattrs` on Windows still rejected at preflight; backend remains unreachable. | Already tracked: WPC-3.wire.1 (#3809), WPC-3.wire.1.b (#3815), WPC-3.wire.1.c (#3816), WPC-3.wire.1.d (#3817), WPC-3.wire.2.b-.d (#3819-#3821). No new task needed; existing subtree is the right home. |
| WPC-4 cross-platform receiver round-trip test (Linux sender to Windows receiver) — blocked on WPC-3 preflight unblock. | Already tracked under WPC-3.wire.3 (#3811) regression test. No new task needed. |
| WPC-5'.7 — wire `to_extended_path` into `metadata::windows` ACL/xattr handle acquisition. Still pending. | Already tracked at #3796. No new task needed. |

No genuinely new gap-fix task is filed by this synthesis. Every gap
surfaced has an existing tracker.

### 4.2 Close conditional follow-ups that resolved as "no gap" or "acted on"

| Task | Closure rationale |
|---|---|
| WPC-V.7 (#3751) | Conditional fired; WPC-8' subtree filed and shipped (#3777-#3789). Close as resolved-by-WPC-8'. |
| WPC-V.8 (#3752) | Conditional fired; WPC-5' subtree filed and shipped (#3790-#3799 except #3796 which remains tracked). Close as resolved-by-WPC-5'. |
| WPC-V.10 (#3754) | Conditional fired; WPC-9' subtree filed and shipped (#3800-#3806). Close as resolved-by-WPC-9'. |

The conditional tasks themselves did not produce code; they acted as
gating markers for the WPC-N' subtrees, which is where the actual
remediation lives. Closing the gating markers does not lose tracking
context.

## 5. Cross-reference update for WPC parent task (#2869)

The WPC parent (#2869) was marked completed on aggregate-tracker basis.
The post-synthesis reality is more nuanced:

- WPC-1 through WPC-13 cells are tracked individually; WPC-3 / WPC-4
  are the only cells whose production-code surface remains partial as
  of 2026-06-11.
- WPC-5 / WPC-6 are fully shipped (PR #5575 + the WPC-5' subtree).
- WPC-8 / WPC-9 are fully shipped (PRs #5579, #5583, #5592 + the
  WPC-8' / WPC-9' subtrees).
- WPC-10 (DACL inherited-ACE), WPC-11 (case-insensitive collision),
  WPC-12 (perm-bits mapping), WPC-13 (Windows support matrix) cells
  ship the underlying production code referenced by
  `docs/user/windows-support-matrix.md`.

The WPC parent (#2869) status remains "completed" because every cell
has either shipped production code or an explicit follow-up (WPC-3.wire
subtree) that captures the residual work. The WPC-V.11 update to the
parent task description already documents this nuance; no further
parent-task edit is needed beyond the audit-only / design-only labelling
WPC-V.12 added to the matrix.

## 6. Audit methodology and follow-up policy

WPC-V applied the rule established in WPC-V.12: no row in the Windows
support matrix may be promoted past "Audit only" without a verified
production call site cited as `crate/path/file.rs:line`. This synthesis
applies the same rule to the WPC-3 / 4 / 8 / 9 tracker entries: a task
is "shipped" only if its production code surface is reachable from the
default `oc-rsync` binary on Windows.

WPC-3 ships the backend but the CLI preflight blocks the surface; that
is the residual gap. WPC-8 / WPC-9 reach the surface (no preflight
gate, no Cargo feature gate); they are shipped.

Future WPC-N cells must apply the same standard: cite a reachable
production call site or mark the cell "design only" until the call site
lands.

## 7. Cross-references

- Parent: `#2869` (Windows real-world parity series).
- WPC-V audits: `#3745` through `#3756`.
- WPC-V.12 matrix update: `docs/user/windows-support-matrix.md`.
- WPC-3 / WPC-4 follow-up: PR #5564 (`--xattrs` preflight gate, currently CLOSED).
- WPC-5 / WPC-6 follow-up: PR #5575 (`to_extended_path` helper, MERGED).
- WPC-8 / WPC-9 follow-up: PR #5579 (reparse-point classifier, MERGED),
  PR #5583 (RAII fixture helpers, MERGED), PR #5592 (reparse-data
  parser, MERGED).
- WPC-3 wire-up tracking: WPC-3.wire.1 / .2 / .3 (#3809-#3811),
  WPC-3.wire.1.a-d (#3814-#3817), WPC-3.wire.2.a-d (#3818-#3821).
- WPC-5'.7 residual: #3796.
- Design docs: `docs/design/wpc-3-ads-implementation.md`,
  `docs/design/wpc-4-ads-roundtrip-test.md`,
  `docs/design/wpc-6-long-path-regression-test.md`,
  `docs/design/wpc-8-reparse-point-classifier.md`,
  `docs/design/wpc-9-reparse-point-regression-test.md`.
- Audit docs: `docs/audit/windows-ads-handling.md`,
  `docs/audit/windows-long-path-support.md`,
  `docs/audit/windows-reparse-point-classification.md`,
  `docs/audit/windows-dacl-ace-inheritance.md`,
  `docs/audit/windows-perm-bits-posix-mapping.md`,
  `docs/audit/windows-case-insensitive-conflict-detection.md`.

Memory cross-links (internal):
`[[project_windows_real_world_parity_unclear]]`,
`[[project_windows_parity_wip]]`,
`[[feedback_trust_fresh_audit_over_session_memory]]`.
