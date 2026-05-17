# Windows hardlink ACL inheritance audit

Tracking issue: #2311. Verified 2026-05-18 against `origin/master`.

## 1. Background

On NTFS, hardlinks point to a single MFT record. The DACL lives on the
inode (the file record), not on the directory entry. When a process
calls `SetNamedSecurityInfoW(path, DACL_SECURITY_INFORMATION, ...)` the
write lands on the shared inode regardless of which alias name was
opened. Two consequences follow:

- Applying the same DACL N times to N hardlink aliases produces N
  identical inode-level writes. The end-state is correct but each
  write performs the full `SetNamedSecurityInfo` round trip (open
  handle, set security info, close).
- Applying *different* DACLs through different aliases is racy: the
  last writer wins. Cohort members must therefore agree on the DACL or
  the outcome depends on completion ordering.

Upstream rsync on Cygwin treats hardlink followers identically to any
other inode-sharing platform: `hlink.c::hard_link_check()` calls
`maybe_hard_link()` which itself only calls `atomic_create()` ->
`do_link()` and then itemizes. It never calls `set_file_attrs()` on
the follower, so `set_acl()` is never invoked for the alias.
`generator.c:1540` returns the follower out of the per-file loop
before `set_file_attrs()` could fire. The leader's `set_acl()` write
populates the shared inode and every follower inherits the DACL for
free.

## 2. oc-rsync apply sites

The audit covers every site that may call ACL apply for a file that
participates in a hardlink cohort.

### 2.1 Wire receive path (network or daemon transfer)

| Site | File | Behaviour | Per-inode or per-file? |
|------|------|-----------|------------------------|
| Transferred leader | `crates/transfer/src/receiver/transfer.rs:454-472` | After rename, applies metadata, xattrs, then `apply_acls_from_receiver_cache`. | Per-leader (one inode). |
| Disk-commit dispatch | `crates/transfer/src/disk_commit/process.rs:411-452` | `apply_metadata_acls_and_xattrs` runs on the file the commit thread just renamed. Only leaders reach this stage; followers are not pushed through the pipeline. | Per-leader. |
| Quick-check match | `crates/transfer/src/receiver/transfer/candidates.rs:179-191` | Up-to-date file: applies metadata + ACL + xattrs. Followers are filtered out at line 71 (`!is_hardlink_follower(e)`). | Per-leader. |
| Reference-dir hard link | `crates/transfer/src/receiver/quick_check.rs:179-191` | `--link-dest` hard-links the reference file into place; ACL is applied to the new alias. | Per-alias (see 3.2). |
| Hardlink follower creation | `crates/transfer/src/receiver/directory/links.rs:146-307` | `create_hardlinks` calls `fast_io::hard_link(leader, follower)` and itemizes. No ACL apply, no metadata apply. | **Inode-aware: ACL skipped.** |
| Directory metadata pass | `crates/transfer/src/receiver/directory/creation.rs:163-170,355` | Applies ACL to directories only; directories never participate in hardlink cohorts. | N/A. |

The receiver's hardlink follower path is already cygwin-equivalent:
the leader's DACL is written exactly once and the followers inherit
through the shared inode.

### 2.2 Local-copy executor

The local-copy executor (`engine::local_copy`) deduplicates by source
`(dev, ino)` via `HardLinkTracker` and creates the destination as a
hardlink whenever the source is a known cohort member.

| Site | File | Behaviour | Per-inode or per-file? |
|------|------|-----------|------------------------|
| Source-side leader (first copy) | `crates/engine/src/local_copy/context_impl/state.rs:230-303` | Full data copy; applies metadata, xattrs, ACL once; records destination in `HardLinkTracker`. | Per-leader. |
| Source-side follower (cached inode) | `crates/engine/src/local_copy/executor/file/copy/links.rs:147-214` | `existing_hard_link_target()` hits; `create_hard_link()` then itemize. **No `sync_acls_if_requested`.** | **Inode-aware: ACL skipped.** |
| `--link-dest` follower | `crates/engine/src/local_copy/executor/file/copy/links.rs:70-145` | `link_dest_target()` hits; `fast_io::hard_link`; itemize and return. **No `sync_acls_if_requested`.** | **Inode-aware: ACL skipped.** |
| `--copy-dest` Link branch | `crates/engine/src/local_copy/executor/file/copy/links.rs:290-360` | Creates hard link to reference, then calls `apply_file_metadata_with_options`, `sync_xattrs_if_requested`, and `sync_acls_if_requested`. | **Per-alias** (see 3.2). |
| Quick-check up-to-date | `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:740-762` | Destination already matches: re-applies metadata, xattrs, ACL, then records as hardlink candidate. | Per-leader: only fires once per `(dev, ino)` because subsequent followers exit at link branch 2 before reaching this code. |

### 2.3 Windows DACL writer

`crates/metadata/src/acl_windows.rs::apply_rsync_acl_to_path` builds a
self-relative DACL, drops unmappable SIDs, and calls
`SetNamedSecurityInfoW(SE_FILE_OBJECT, DACL_SECURITY_INFORMATION)`.
The implementation does not check `nlink`; it has no notion of cohort
membership. Idempotence is guaranteed only when the input
`RsyncAcl` is bit-for-bit identical across calls.

## 3. Risk analysis

### 3.1 Wire path (receiver) - no risk

`create_hardlinks` never applies metadata or ACLs to followers.
Behaviour matches upstream cygwin: the leader writes the shared DACL
once and the followers inherit through the inode. There is no
duplicate write and no race because no second writer exists.

### 3.2 Local-copy `--copy-dest` Link branch - duplicate writes, idempotent

When the executor takes the `--copy-dest` `ReferenceDecision::Link`
branch (`links.rs:290-360`) it links every follower to the same
reference inode and then re-applies the source's ACL on each
follower. The DACL is identical for every cohort member (read from
the same source), so:

- The end state is correct.
- Each follower triggers a redundant
  `SetNamedSecurityInfoW` round trip on the already-correct inode.
- There is no true race: all writes carry the same bytes, so
  last-writer-wins still yields the intended DACL.

Cost: O(N) `SetNamedSecurityInfo` calls instead of O(1). For a
50-link cohort this is ~50 redundant Win32 calls and the associated
audit log noise (`SE_AUDIT_PRIVILEGE` writes show every change).

### 3.3 Local-copy `--link-dest` branch - no risk

`process_links` branch 1 (`links.rs:70-145`) does not call
`sync_acls_if_requested`. The ACL on the link-dest inode is preserved
as-is. This matches the upstream contract for `--link-dest`: the
target acts as a reference and its existing metadata must be kept.

### 3.4 Concurrent follower creation - benign race window

`create_hardlinks` is single-threaded over `self.file_list` and
`process_links` is invoked from the local-copy executor's per-entry
loop. Neither runs ACL apply for followers, so no race exists in the
common path. The `--copy-dest` Link branch (3.2) does apply ACL but
all writers write identical bytes; the race window is harmless.

If the local-copy executor were ever parallelised across cohort
members and the `--copy-dest` Link branch were taken concurrently,
the same identical-DACL invariant still protects correctness.

### 3.5 Cross-cohort interference - not possible

`HardLinkTracker` keys entries by `(dev, ino)` so distinct cohorts
cannot alias. The receiver-side tracker (`HardlinkApplyTracker`) keys
by protocol `gnum`; distinct cohorts get distinct gnums on the wire.

## 4. Comparison to upstream cygwin behaviour

| Aspect | Upstream rsync 3.4.2 (cygwin) | oc-rsync wire receiver | oc-rsync local-copy |
|--------|-------------------------------|------------------------|---------------------|
| Leader ACL write | Once, via `set_file_attrs` -> `set_acl`. | Once, via `apply_acls_from_receiver_cache`. | Once, via `sync_acls_if_requested`. |
| Follower ACL write | Never (`hard_link_check` returns 1, generator loop exits at `goto cleanup;`). | Never (`create_hardlinks` skips ACL apply). | Never on cached-inode and `--link-dest` branches; once-per-follower on `--copy-dest` Link branch. |
| Relies on inode sharing for ACL? | Yes. | Yes. | Yes for the cached-inode branch; redundant for `--copy-dest`. |
| Race window | None. | None. | None (identical-DACL invariant). |

The wire receiver matches upstream byte-for-byte. The local-copy
`--copy-dest` Link branch is the only divergence; the divergence is a
performance cost, not a correctness defect.

## 5. Recommendations

### 5.1 Keep the current "skip follower" model (preferred)

The receiver's `create_hardlinks` and the local-copy cached-inode
branch already implement the upstream contract: leader gets the write,
followers inherit. This is correct on NTFS because DACLs live on the
inode and is required on POSIX with hardlinks for the same reason.
Do not add ACL apply calls to either follower path.

### 5.2 Skip the redundant `--copy-dest` Link branch write on followers

For `crates/engine/src/local_copy/executor/file/copy/links.rs:290-360`,
gate the `apply_file_metadata_with_options` / `sync_xattrs_if_requested`
/ `sync_acls_if_requested` block on
`!context.is_hardlink_follower(metadata)`. The simplest signal is
`metadata.nlink() > 1 && context.existing_hard_link_target(metadata).is_some()`
on Unix; on Windows query the same via `GetFileInformationByHandle`
(`nNumberOfLinks > 1`) or simply rely on the destination's inode
sharing with another cohort member already recorded in the tracker.
When skipped, leave a `// upstream: hlink.c - inode shares DACL`
comment so future audits do not regress.

### 5.3 Defensive `IsValidSid` short-circuit

`apply_rsync_acl_to_path` already drops unmappable SIDs. No extra
inode-level guard is needed because the call is already idempotent
when input DACLs match.

## 6. Test plan

A 3-link cohort exercises every branch.

### 6.1 Wire receive (Windows host, daemon transfer)

```text
setup:
  src/leader.bin           (16 KiB random, ACL: Everyone:R, Admins:F)
  src/follow1.bin          (hardlink to leader.bin)
  src/follow2.bin          (hardlink to leader.bin)

run:
  oc-rsync -aHX --acls src/ rsync://host/dst/

verify:
  - dst/leader.bin, dst/follow1.bin, dst/follow2.bin share one NTFS
    inode (FileIndex via GetFileInformationByHandle).
  - DACL on the shared inode contains Everyone:R and Admins:F.
  - Process Monitor capture shows exactly one
    SetNamedSecurityInfoW write on the cohort inode.
  - SetSecurityInfo call count == 1, not 3.
```

### 6.2 Local copy (Windows host, no daemon)

```text
setup:
  same cohort layout under src\

run:
  oc-rsync -aHX --acls src\ dst\

verify:
  - dst\leader.bin, dst\follow1.bin, dst\follow2.bin share one inode.
  - DACL correct.
  - SetSecurityInfo call count == 1.
```

### 6.3 Local copy with `--copy-dest`

```text
setup:
  ref\leader.bin + hardlinks ref\follow1.bin, ref\follow2.bin
  src\leader.bin + hardlinks src\follow1.bin, src\follow2.bin
    (different content, different ACL)

run:
  oc-rsync -aHX --acls --copy-dest=ref src\ dst\

current behaviour: SetSecurityInfo call count == 3 (one per follower).
post-5.2 behaviour: SetSecurityInfo call count == 1.

verify in both cases:
  - Final DACL on cohort inode == src\leader.bin DACL.
  - All three dst entries share one inode.
```

### 6.4 Cross-cohort isolation

```text
setup:
  cohort A: src\a1, src\a2 (linked, DACL A)
  cohort B: src\b1, src\b2 (linked, DACL B)

run:
  oc-rsync -aHX --acls src\ dst\

verify:
  - dst\a1, dst\a2 share inode I_a with DACL A.
  - dst\b1, dst\b2 share inode I_b with DACL B.
  - SetSecurityInfo count == 2.
```

### 6.5 Property test (cross-platform)

Add `crates/engine/src/local_copy/executor/file/copy/links.rs` test:
build N-element cohort, run copy with `--acls` mocked via a counter,
assert the counter equals 1 regardless of N. Run the same test for
both Unix (`nlink > 1`) and a Windows mock that returns
`nNumberOfLinks > 1`.

## 7. Out of scope

- SACL handling (`acl_windows.rs` skips it by design; `SE_SECURITY_NAME`
  privilege is not requested).
- POSIX default ACLs on directories (Windows has no analogue).
- Inheritance flag round-tripping (deferred to Tier 2 ACL work).
