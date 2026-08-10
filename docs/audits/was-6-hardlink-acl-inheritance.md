# WAS-6: Windows hardlink ACL inheritance audit

Tracking issue: #2311. Verified 2026-05-18 against `origin/master`.
Companion to the prior structural audit at
`docs/audits/windows-hardlink-acl-inheritance.md`, which catalogues every
ACL apply site. This document focuses on the WAS-6 question:

> When `--acls` and `--hard-links` are both passed and the source
> contains a hardlink set, what does oc-rsync do on Windows today and
> how does it diverge from upstream rsync's Linux behaviour?

## 1. Today's behaviour on Windows

`crates/metadata/src/acl_windows.rs` is the only DACL writer on Windows.
Every apply call funnels into `apply_rsync_acl_to_path` at
`crates/metadata/src/acl_windows.rs:529`, which converts the
`RsyncAcl` to a DACL and writes it through
`SetNamedSecurityInfoW(SE_FILE_OBJECT, DACL_SECURITY_INFORMATION)` at
`crates/metadata/src/acl_windows.rs:614`. The implementation does not
consult `nNumberOfLinks` and has no cohort awareness. Because NTFS keys
the security descriptor off the MFT inode rather than the directory
entry, a write through any alias updates the descriptor for every alias
in the cohort.

On the wire-receiver path, `create_hardlinks` at
`crates/transfer/src/receiver/directory/links.rs:146-307` creates each
follower via `fast_io::hard_link(&leader_path, &link_path)` at
`crates/transfer/src/receiver/directory/links.rs:261` and never calls
`apply_acls_from_receiver_cache` or any metadata apply for that
follower. The leader is fully written and committed through the normal
receiver pipeline at
`crates/transfer/src/receiver/transfer.rs:454-472`, including
`apply_acls_from_receiver_cache` at
`crates/transfer/src/receiver/mod.rs:686-708`. The follower then
inherits the leader's DACL via the shared NTFS inode. Quick-check
matches for leaders run through
`crates/transfer/src/receiver/quick_check.rs:184` and
`crates/transfer/src/receiver/transfer/candidates.rs:179`, both of
which apply ACLs to the leader path. Followers are filtered out before
those branches by `is_hardlink_follower`.

On the local-copy executor path, `process_links` at
`crates/engine/src/local_copy/executor/file/copy/links.rs:47-382`
handles three follower branches. Branches 1 and 2 (`--link-dest` and
the cached-inode path at lines 70-145 and 147-214) create the hardlink
and exit without calling `sync_acls_if_requested`; the link inherits
the source's DACL through inode sharing. The `--copy-dest` Link
branch at lines 290-373 differs: it creates the link, then explicitly
calls `apply_file_metadata_with_options`, `sync_xattrs_if_requested`,
and `sync_acls_if_requested(preserve_acls, mode, source, destination,
true)` at lines 329-341. Because all followers in a cohort target the
same NTFS inode, the executor issues one `SetNamedSecurityInfoW` per
follower path even though the inode already holds the correct DACL.

## 2. Upstream rsync behaviour on Linux

Upstream sends ACL data per file entry, not per inode. `flist.c:1628`
calls `get_acl()` for every non-symlink entry during file-list build,
and `flist.c:1653-1656` calls `send_acl(f, &sx)` after every
`send_file_entry`. The receiver caches ACLs in `access_acl_list` and
records `F_ACL(file)` per entry, so each follower carries its own ACL
index on the wire. This is identical for leaders and followers and is
not gated on `FLAG_HLINKED`.

When the generator processes a follower, it invokes
`hard_link_check()` at `upstream:generator.c:1540`. The check calls
`maybe_hard_link()` at `upstream:hlink.c:210-242`, which either
(a) returns 0 with `FLAG_HLINK_DONE` set when the destination already
shares the leader's inode (`upstream:hlink.c:215-227`) or (b) calls
`atomic_create()` to create the link and returns 0
(`upstream:hlink.c:230-238`). Neither branch invokes
`set_file_attrs()` on the follower path. Control returns to
`generator.c:1541` which executes `goto cleanup;`, bypassing the
`set_file_attrs(fname, file, &sx, NULL, maybe_ATTRS_REPORT)` call at
`upstream:generator.c:1814` that fires for non-hardlinked files. The
follower's per-entry ACL index is therefore never consumed for the
follower path; the leader's `set_file_attrs` -> `set_acl` chain
(`upstream:rsync.c:653`) updated the shared inode and that update is
visible through every alias.

`set_acl()` at `upstream:acls.c:1013-1057` honours `F_ACL(file)` and
calls `set_rsync_acl(..., SMB_ACL_TYPE_ACCESS, ...)`. On POSIX,
`sys_acl_set_file` writes to the inode just like
`SetNamedSecurityInfoW` does on NTFS, so a single leader-side write
covers every alias. Upstream's wire encoding still carries
follower-specific ACL payloads, but the receiver discards them in
practice because `hard_link_check` short-circuits the follower's
`set_file_attrs` path.

## 3. Divergence

- **Wire receive path (`-aHX --acls` over network or daemon):** No
  divergence. oc-rsync's `create_hardlinks` at
  `crates/transfer/src/receiver/directory/links.rs:146` follows the
  upstream contract: write the leader's DACL once via
  `apply_acls_from_receiver_cache`, let NTFS propagate to followers.
  Process Monitor captures should show exactly one
  `SetNamedSecurityInfoW` per cohort inode, matching the upstream
  `set_acl` count.

- **Local-copy `--link-dest` and cached-inode branches:** No
  divergence. Branches 1 and 2 at
  `crates/engine/src/local_copy/executor/file/copy/links.rs:70-214`
  create the link without re-applying ACLs. Upstream's `try_dests_reg`
  at `upstream:generator.c:991-1002` follows the same pattern: call
  `hard_link_one`, optionally `set_file_attrs` only when
  `atimes_ndx` is set, then `finish_hard_link`. No ACL re-application.

- **Local-copy `--copy-dest` Link branch:** Divergence in cost, not
  in correctness. oc-rsync re-applies the source ACL to every
  follower at
  `crates/engine/src/local_copy/executor/file/copy/links.rs:341`. The
  NTFS inode already holds the correct DACL after the first apply, so
  the follower-side writes are redundant
  `SetNamedSecurityInfoW` round trips. Upstream avoids this entirely
  because `hard_link_check` returns control to the generator's
  `goto cleanup;` before the `--copy-dest` `set_file_attrs` site at
  `upstream:generator.c:1814` ever runs for followers. Net effect:
  oc-rsync issues O(N) inode writes per N-link cohort under
  `--copy-dest`; upstream issues O(1).

- **Wire encoding:** Both implementations transmit per-entry ACL
  payloads for followers and both rely on inode sharing to make those
  payloads no-ops at apply time. No wire divergence.

- **Cross-platform symmetry:** On POSIX targets `acl_exacl` follows
  the same skip-follower model as the Windows path. The Windows
  `acl_windows::apply_rsync_acl_to_path` differs from the Linux
  `acl_exacl::apply_acls_from_cache` only in the underlying syscall
  (`SetNamedSecurityInfoW` vs `acl_set_file`); both touch inode-level
  metadata, so the upstream contract holds on both platforms.

## 4. Recommendations

1. **Hold the current "skip follower" model in the wire and
   `--link-dest`/cached-inode paths.** Both already match upstream
   byte-for-byte. Add a regression test at
   `crates/transfer/src/receiver/directory/links.rs` that counts
   `SetNamedSecurityInfoW` invocations (mock the writer behind a
   trait) and asserts `count == 1` for a 3-link cohort under
   `-aHX --acls`. See companion audit section 6.1 for the test
   layout.

2. **Gate the redundant `--copy-dest` Link branch follower writes.**
   In `crates/engine/src/local_copy/executor/file/copy/links.rs:328-341`,
   wrap the `apply_file_metadata_with_options` /
   `sync_xattrs_if_requested` / `sync_acls_if_requested` block in a
   follower check. The check should be:
   - On Unix: `metadata.nlink() > 1 &&
     context.existing_hard_link_target(metadata).is_some()`
     (the tracker already records this).
   - On Windows: query `nNumberOfLinks > 1` via
     `GetFileInformationByHandle` and consult the executor's
     `HardLinkTracker` for cohort membership. The Windows tracker
     stub at `crates/engine/src/local_copy/hard_links.rs:227-242`
     would need a real implementation; today it returns `None` for
     every query so the gate would fail open.
   Add a `// upstream: hlink.c - inode shares DACL; do not re-apply
   on follower` comment at the gate.

3. **Strengthen the Windows `HardLinkTracker` stub** at
   `crates/engine/src/local_copy/hard_links.rs:227-242`. Today the
   Windows path uses a unit struct that records nothing, so the
   local-copy executor cannot detect cohort membership on Windows.
   Implement a real tracker keyed by `(volume_serial, file_index)`
   from `BY_HANDLE_FILE_INFORMATION` so recommendation 2's follower
   gate has correct input. File: same module, lines 227-242.

4. **Add a property test** at
   `crates/engine/src/local_copy/executor/file/copy/links.rs` that
   builds an N-element cohort, executes the local-copy executor with
   `--acls --copy-dest=ref`, and asserts the ACL writer was called
   exactly once. Use the existing test harness pattern from
   `crates/engine/src/local_copy/tests/execute_hardlinks.rs`.

5. **Document the inode-share invariant** in
   `crates/metadata/src/acl_windows.rs` module docs (top of file,
   currently lines 1-43). Add a sentence stating that callers must
   not invoke `apply_rsync_acl_to_path` per alias when the cohort
   already holds the desired DACL; the function has no internal
   short-circuit.

## 5. Out of scope

- SACL handling (`acl_windows.rs` skips it by design; see
  `crates/metadata/src/acl_windows.rs:16-22`).
- POSIX default ACLs (no NTFS analogue; see
  `crates/metadata/src/acl_windows.rs:374-379`).
- Inheritance flag round-tripping (deferred to Tier 2 ACL work).
- ReFS, FAT32, and network-mount fallbacks; `read_dacl` already
  returns `Ok(null_dacl)` for unsupported volumes at
  `crates/metadata/src/acl_windows.rs:180-183`.
