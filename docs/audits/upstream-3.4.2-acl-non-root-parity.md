# ACL ID mapping for non-root receivers vs rsync 3.4.2

Tracking issues: #2230, #618. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

The rsync 3.4.2 NEWS file records:

> Fixed ACL ID mapping for non-root users (closes #618).

The fix is a single-line gate removal in `acls.c::recv_ida_entries()`:

```diff
--- rsync-3.4.1/acls.c
+++ rsync-3.4.2/acls.c
@@ -713,7 +713,7 @@ static uchar recv_ida_entries(int f, ida_entries *ent)
                        else
                                id = recv_group_name(f, id, NULL);
                } else if (access & NAME_IS_USER) {
-                       if (inc_recurse && am_root && !numeric_ids)
+                       if (inc_recurse && !numeric_ids)
                                id = match_uid(id);
                } else {
                        if (inc_recurse && (!am_root || !numeric_ids))
```

Before the fix the receiver only called `match_uid()` for user IDs when
the destination process ran as root. Non-root receivers in incremental
recursion mode therefore stored the raw sender-side numeric UID in the
ACL entry. When `set_acl()` later handed the ACL to `sys_acl_set_file()`
the kernel could reject the unknown UID with `EPERM` or `EINVAL`, which
upstream surfaces as a transfer-level error from `set_rsync_acl()`
(`acls.c` line 993-996). The group-side path (line 719) already lacked
the `am_root` guard, which is why this audit focuses on the user-ID
branch.

Removing the `am_root` term lets the receiver remap unknown sender UIDs
to the local view via `uidlist.c::match_uid()`, which in turn falls back
to the raw numeric ID when no mapping is known. The remap-then-fallback
chain is identical for root and non-root receivers in 3.4.2.

## 2. oc-rsync surface area

### 2.1 ACL receive path

`crates/protocol/src/acl/wire/recv.rs::recv_ida_entries` (lines 27-55)
mirrors upstream `acls.c::recv_ida_entries` lines 697-729 but contains
no equivalent of the `match_uid`/`match_gid`/`recv_user_name`/
`recv_group_name` branches. Every ACL `id_access` entry is stored with
the raw sender numeric ID and, when `XFLAG_NAME_FOLLOWS` was set, with
the trailing name bytes left unresolved in `IdAccess::name:
Option<Vec<u8>>`.

This applies uniformly to both incremental and non-incremental file-list
modes: oc-rsync never performs a post-flist `match_acl_ids()` pass
(upstream `acls.c` lines 1061-1081) either.

### 2.2 ACL apply path

`crates/metadata/src/acl_exacl.rs::apply_acls_from_cache` is the only
receiver-side apply site (called from `crates/transfer/src/disk_commit/
process.rs::apply_metadata_acls_and_xattrs` line 421 and from
`crates/transfer/src/receiver/mod.rs` line 564).

When converting `RsyncAcl` to `exacl::AclEntry`s,
`rsync_acl_to_entries` (`acl_exacl.rs` lines 511-558) calls
`ida.id.to_string()` and feeds the numeric string straight into
`AclEntry::allow_user`/`allow_group`. `exacl::setfacl` then asks the
kernel to install the ACL.

### 2.3 Error handling fall-through

`is_unsupported_error` (`acl_exacl.rs` lines 427-444) treats `EPERM`,
`ENOTSUP`, `ENOENT`, `EINVAL` and `ENODATA` as "no ACL support" and
short-circuits the failure path at `apply_acls_from_cache` lines
605-616 (access ACL) and 624-636 (default ACL). The receiver therefore
drops the unrepresentable ACL but keeps the file and continues the
transfer.

`Receiver::match_uid` / `Receiver::match_gid` (`transfer/src/receiver/
mod.rs` lines 376-394) exist and are wired to `uid_list`/`gid_list`,
but they are only consulted from the ownership apply path. They are
never invoked for ACL `id_access` entries.

## 3. Parity assessment

| Behaviour | Upstream 3.4.2 | oc-rsync today |
| --- | --- | --- |
| Non-root receiver remaps user IDs in inc-recurse ACL stream | yes (`match_uid` per entry) | no remap, raw sender UID stored |
| Non-root receiver remaps group IDs in inc-recurse ACL stream | yes (already non-root since pre-3.4.2) | no remap, raw sender GID stored |
| Name-follows entries resolved to local UID via `getpwnam`-equivalent | yes (`recv_user_name`) | no, name bytes stored unused |
| Apply-time failure when remap is missing and kernel rejects ID | hard error: `set_rsync_acl` returns -1 | soft fall-through: `EPERM`/`EINVAL` swallowed, ACL dropped, transfer continues |
| Numeric-ids passthrough (`--numeric-ids`) | bypasses remap | same effective outcome (no remap is performed) |

The end-user impact differs in shape rather than severity:

- Upstream's pre-3.4.2 bug surfaced as a transfer error code on
  ACL-bearing files. The 3.4.2 fix turns those errors into a successful
  remap-or-numeric-passthrough.
- oc-rsync never errors in the first place because the apply-time
  EPERM fall-through silently drops the ACL. The receiver still does
  not remap, so when a remap *would* have produced a working local ID
  (e.g. matching name on both sides) oc-rsync still drops the entry
  instead of preserving it.

Because the practical failure modes (ACL drop on permission error vs
transfer failure) are observably distinct from upstream and depend on
non-root execution, the gap is worth documenting even though it does
not break transfers.

## 4. Remediation outlook

A faithful match-then-fallback implementation would:

1. Plumb the receiver's `uid_list`/`gid_list` view down into
   `recv_ida_entries` (or perform a post-flist remap pass equivalent to
   upstream `match_racl_ids`).
2. Resolve `XFLAG_NAME_FOLLOWS` names via `getpwnam_r`/`getgrnam_r`
   when `!numeric_ids`, with a numeric fallback on lookup failure.
3. Hand the remapped numeric ID to `exacl::AclEntry::allow_user` as the
   numeric-string form `exacl` already accepts.

These changes touch `protocol::acl`, `metadata::acl_exacl`, and
`transfer::receiver` (to pass the id-mapping context). They also
require a non-root receiver test fixture because the failure only
manifests when the destination's effective UID cannot install the
sender's UID directly. The remediation is tracked under #618 and is
out of scope for this documentation-only parity audit.

## 5. Conclusion

oc-rsync does not implement the 3.4.2 ACL ID-mapping fix verbatim, but
the apply-time `EPERM` fall-through in `apply_acls_from_cache` prevents
the pre-3.4.2 transfer-failure symptom. The behavioural delta -
silently dropping unmapped ACL entries instead of remapping them - is
documented here for follow-up under #618.

Upstream reference: `acls.c::recv_ida_entries` line 716, `uidlist.c::
match_uid` line 297, `acls.c::match_racl_ids` line 1061. Local copy at
`target/interop/upstream-src/rsync-3.4.2/acls.c`.
