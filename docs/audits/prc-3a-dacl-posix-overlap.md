# PRC-3a: PR #4361 DACL <-> POSIX overlap with master WAS-5

## Context

- PR #4361 (`feat/dacl-posix-mapping`, branch tip vs `master` at audit time) implements
  WAS-4 (#2309): the `dacl_to_posix_mode` / `posix_mode_to_dacl` helpers plus an
  SDDL grammar (`split_sddl`, `parse_aces`, `sddl_rights_to_perms`,
  `perms_to_sddl_rights`) and the `SDDL_EVERYONE` / `SDDL_AUTHENTICATED_USERS`
  constants in `crates/metadata/src/acl_windows.rs`.
- PR #4388 (WAS-5, merged into `master` on 2026-05-18) brought in **both** the
  WAS-4 helpers and the WAS-5 wiring (`read_dacl_sddl`, `write_dacl_sddl`,
  `read_sddl_with_sacl`, `sddl_xattr_entry`, `find_sddl_in_xattrs`,
  `apply_sddl_from_xattrs`, `WINDOWS_SDDL_XATTR_NAME`, `io_error_is_unsupported`,
  the `OwnedLocalWString` RAII guard, and the `sync_acls` SDDL preferred-path).
  Its PR body says exactly this: "Brings the WAS-4 `dacl_to_posix_mode` /
  `posix_mode_to_dacl` helpers in alongside the WAS-5 wiring so the dispatch
  shape is self-contained; this PR can land before or after #4361 (overlapping
  definitions will fold cleanly)."

The mechanical consequence: `master`'s `acl_windows.rs` is **a superset** of
the branch's `acl_windows.rs`. Every byte the branch wants to add is already
present in `master`, modulo a handful of cosmetic doc/comment edits.

## Method

```sh
git fetch origin feat/dacl-posix-mapping --quiet
git show origin/master:crates/metadata/src/acl_windows.rs > master.rs
git show origin/feat/dacl-posix-mapping:crates/metadata/src/acl_windows.rs > branch.rs
git show $(git merge-base origin/master origin/feat/dacl-posix-mapping):crates/metadata/src/acl_windows.rs > base.rs
diff -u master.rs branch.rs > diff.txt
diff -u base.rs branch.rs > base_vs_branch.txt
diff -u base.rs master.rs > base_vs_master.txt
```

- Merge base: `0cd89bca217fef2fc213823aa3d59b1c974392eb`.
- `base.rs`: 827 lines. `branch.rs`: 1232 lines (+405). `master.rs`: 1761 lines (+934).
- `master.rs` vs `branch.rs`: 11 unified-diff hunks. (GitHub's mergeability check
  may report a higher hunk count when split around the merge-base; the conflict
  surface in `acl_windows.rs` resolves to the 11 logical regions enumerated
  below. Hunks are numbered in source order.)
- `lib.rs` re-exports were checked separately:
  - `master` re-exports `WINDOWS_SDDL_XATTR_NAME, apply_sddl_from_xattrs,
    dacl_to_posix_mode, find_sddl_in_xattrs, posix_mode_to_dacl, read_dacl_sddl,
    read_sddl_with_sacl, sddl_xattr_entry, write_dacl_sddl`.
  - `branch` re-exports only `dacl_to_posix_mode, posix_mode_to_dacl`.

## Per-hunk table

| # | Hunk (master line range) | (a) What the branch wants to add | (b) Already on master via WAS-5? | (c) Recommended resolution |
|---|---|---|---|---|
| 1 | `@@ -51,22 +51,15 @@` (imports) | Branch **removes** WAS-5 imports: `XattrEntry`, `XattrList`, `ConvertSecurityDescriptorToStringSecurityDescriptorW`, `ConvertStringSecurityDescriptorToSecurityDescriptorW`, `SDDL_REVISION_1`, `GetSecurityDescriptorDacl/Group/Owner/Sacl`, `OBJECT_SECURITY_INFORMATION`, `OWNER_SECURITY_INFORMATION`, `PROTECTED_DACL_SECURITY_INFORMATION`, `SACL_SECURITY_INFORMATION`, `GROUP_SECURITY_INFORMATION`. (Branch never added these; master added them via WAS-5.) | Yes - master already needs and uses every removed symbol. | **take-master** |
| 2 | `@@ -436,23 +429,6 @@` (`sync_acls` SDDL preferred path) | Branch **removes** the `read_dacl_sddl` -> `write_dacl_sddl` SDDL round-trip branch and the `io_error_is_unsupported` fallback check inside `sync_acls`. | Yes - this is the WAS-5 preferred path; master keeps it ahead of the lossy named-ACE encoder. | **take-master** |
| 3 | `@@ -708,285 +684,6 @@` (SDDL read/write block) | Branch **removes** 285 lines of WAS-5 infrastructure: `OwnedLocalWString` RAII guard, `sddl_security_info`, `read_dacl_sddl`, `read_sddl_with_sacl`, `read_sddl_internal`, `write_dacl_sddl`. | Yes - entire WAS-5 SDDL read/write API is in master. | **take-master** |
| 4 | `@@ -1036,9 +733,9 @@` (`perms_to_sddl_rights` rustdoc) | Branch reflow of the rustdoc paragraph: "round-trips through [`sddl_rights_to_perms`]." line break placement. Pure formatting. | Yes - same function with same body; only doc line-wrap differs. | **take-master** (or merge-both - identical semantics) |
| 5 | `@@ -1060,15 +757,22 @@` (`split_sddl`) | Branch adds three explanatory inline comments inside `split_sddl`: "Locate each section header...", "Section ends at the next two-character header...", "Header is the character preceding the colon...". Code is byte-equivalent. | Function body is identical on master; only the inline comments are missing. | **merge-both** (cheap to take the branch's comments since they document non-obvious logic) |
| 6 | `@@ -1166,6 +870,7 @@` (`dacl_to_posix_mode` inherited-ACE branch) | Branch adds one inline comment: `// Inherited ACE: not transmitted per design doc section 5.3.` | Function body is identical; comment is missing on master. | **merge-both** (one-line comment, useful upstream-design pointer) |
| 7 | `@@ -1242,101 +947,6 @@` (xattr helpers) | Branch **removes** 101 lines: `WINDOWS_SDDL_XATTR_NAME`, `sddl_xattr_entry`, `find_sddl_in_xattrs`, `apply_sddl_from_xattrs`, `io_error_is_unsupported`. | Yes - all five symbols are the WAS-5 xattr carrier surface and live in master with the matching `lib.rs` re-export. | **take-master** |
| 8 | `@@ -1478,70 +1088,6 @@` (Windows SDDL tests) | Branch **removes** 70 lines of Windows-only tests: `read_dacl_sddl_returns_non_empty_for_temp_file`, `write_dacl_sddl_round_trips_known_descriptor`, `write_dacl_sddl_preserves_owner_and_group`, `write_dacl_sddl_rejects_invalid_input`. | Yes - all four tests exist on master inside the existing `#[cfg(test)] mod tests` block. | **take-master** |
| 9 | `@@ -1587,8 +1133,11 @@` (`posix_mode_to_dacl_uses_three_allow_aces_with_protected_flag` test) | Branch adds three inline comments to assertions: `// owner gets rwx`, `// group gets r-x`, `// other gets r-x via WD`. Assertions are byte-equivalent. | Test body identical on master; comments missing. | **merge-both** (cosmetic) |
| 10 | `@@ -1615,29 +1164,43 @@` (four `dacl_to_posix_mode_*` tests) | Branch refactors each test from a single `assert_eq!(dacl_to_posix_mode(sddl), 0oNNN)` to `let mode = dacl_to_posix_mode(sddl); assert_eq!(mode, 0oNNN)` and adds explanatory comments per test (e.g. "owner BA -> 7, group SY -> 5, other WD -> 4"). Semantics unchanged. | Tests exist on master in the terser form. | **merge-both** (the branch's commented form is more readable; safe to adopt) |
| 11 | `@@ -1666,96 +1229,4 @@` (xattr tests) | Branch **removes** 96 lines of WAS-5 xattr tests: `find_sddl_in_xattrs_returns_payload`, `find_sddl_in_xattrs_returns_none_when_missing`, `find_sddl_in_xattrs_skips_abbreviated_entries`, `apply_sddl_from_xattrs_no_payload_is_noop`, `sddl_xattr_entry_round_trips_on_ntfs`, `sync_acls_prefers_sddl_round_trip`. | Yes - all six tests are in master. | **take-master** |

## Roll-up

- Hunks already fully present on master: **7** (#1, #2, #3, #4, #7, #8, #11). All
  are net removals on the branch side - the branch lacks code that master
  carries via WAS-5.
- Hunks that need merge-both: **4** (#5, #6, #9, #10). All are purely
  documentary additions on the branch (inline comments / test-comment
  annotations). No behavioural change.
- Hunks that need take-branch: **0**. The branch carries nothing functional
  that master does not already have.

## Recommendation

**Close PR #4361 as superseded by master (WAS-5 / #4388).**

Justification:

1. The two public symbols the branch adds (`dacl_to_posix_mode`,
   `posix_mode_to_dacl`) are already shipped on master in their exact byte
   form, exported through `crates/metadata/src/lib.rs`, and covered by the
   identical test suite plus a 0o000..=0o777 round-trip matrix.
2. All other branch content (the SDDL grammar, the constants, the helper
   structs) is also present verbatim on master.
3. A rebase would resolve every functional hunk by taking master, leaving
   only four cosmetic comment-only hunks. The remaining value is small enough
   that it is cheaper to cherry-pick the four comment additions into a tiny
   follow-up `docs:` PR than to rebase #4361 and re-review the whole change.
4. PR #4388's body explicitly anticipates this outcome ("overlapping
   definitions will fold cleanly").

If the four cosmetic comment additions are considered worth preserving, file
a single `docs(metadata): annotate SDDL parser and dacl_to_posix_mode tests`
follow-up that applies hunks #5, #6, #9, #10 to master.

## Follow-ups for PRC-3b / PRC-3c

- PRC-3b should focus on **WAS-6** (hardlink ACL inheritance) and any sibling
  branches whose merge-base predates #4388. Those branches will see the same
  pattern: WAS-4 helper definitions already on master.
- PRC-3c should audit `crates/metadata/src/lib.rs` re-export drift across any
  open Windows-ACL branches to confirm no branch re-introduces a duplicate
  `pub use acl_windows::{dacl_to_posix_mode, posix_mode_to_dacl, ...}` line
  that would collide with master.
