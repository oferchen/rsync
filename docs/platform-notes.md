# Platform Notes

Cross-platform metadata support in oc-rsync. All stubs compile cleanly on
every tier-1 target; no missing `#[cfg]` gates.

## Feature Matrix

| Feature | Linux | macOS | FreeBSD | Windows |
|---------|:-----:|:-----:|:-------:|:-------:|
| POSIX ACLs (`-A`) | yes (`exacl`) | yes (`exacl`) | yes (`exacl`) | no-op + warning |
| NFSv4 ACLs | yes (via xattr) | yes (via xattr) | yes (via xattr) | no-op |
| Extended attributes (`-X`) | yes (`xattr` crate) | yes (`xattr` crate) | yes (`xattr` crate) | no-op + warning |
| uid/gid preservation (`-o`/`-g`) | yes | yes | yes | identity stub |
| Device/FIFO creation (`-D`) | yes | yes | yes | no-op |
| Symlinks (`-l`) | yes | yes | yes | yes |
| Hardlinks (`-H`) | yes | yes | yes | yes |
| Permissions (`-p`) | yes | yes | yes | read-only bit only |
| Sparse files (`-S`) | yes | yes | yes | yes |
| `--fake-super` xattr storage | yes | yes | yes | `Unsupported` error |

## Windows Behavior

### ACLs (`-A`)

When `-A` is passed on Windows, `acl_noop::sync_acls()` emits a **one-time
warning** to stderr and returns `Ok(())` for every file:

> warning: ACLs are not supported on this platform; skipping ACL preservation

This matches upstream rsync, which also skips ACL preservation on platforms
without POSIX ACL headers at compile time.

**Windows equivalent (not implemented):** Windows uses DACLs (Discretionary
Access Control Lists) managed through the Win32 Security API
(`GetNamedSecurityInfoW` / `SetNamedSecurityInfoW`). These are structurally
different from POSIX ACLs — Windows ACLs are always NFSv4-style
(allow/deny per principal) rather than POSIX.1e-style (user/group/other/mask).
Upstream rsync does not support Windows DACLs either.

### Extended Attributes (`-X`)

When `-X` is passed on Windows, `xattr_stub::sync_xattrs()` emits a
**one-time warning** and returns `Ok(())`:

> warning: extended attributes are not supported on this platform; skipping xattr preservation

The `xattr` crate dependency is gated with `cfg(unix)` in `Cargo.toml`, so
it is never compiled on Windows.

**Windows equivalent (not implemented):** NTFS Alternate Data Streams (ADS)
serve a similar role to Unix extended attributes. Access is via the
`filename:streamname` path syntax or `BackupRead`/`BackupWrite` APIs.
Upstream rsync does not support NTFS ADS either.

### NFSv4 ACLs

The NFSv4 ACL module (`nfsv4_acl_stub.rs`) provides type definitions
(`Nfs4Ace`, `Nfs4Acl`, `AceType`, `AceFlags`, `AccessMask`) so code that
references these types compiles on all platforms. All operations return
`Ok(None)`, `Ok(())`, or `false`.

### Ownership (`-o`/`-g`)

`ownership_stub.rs` provides identity functions — uid/gid values pass
through unchanged but are not applied. `id_lookup_stub.rs` returns `None`
for all name-to-id and id-to-name lookups.

### `--fake-super`

`fake_super.rs` returns `ErrorKind::Unsupported` on Windows since it
requires xattr support to store metadata in `user.rsync.%stat` attributes.

## Stub Architecture

```
                   ┌─── Linux/macOS/FreeBSD ──→ acl_exacl.rs (real)
-A flag ──→ cfg ──┤─── iOS/tvOS/watchOS ─────→ acl_stub.rs (warning)
                   └─── everything else ──────→ acl_noop.rs (warning)

                   ┌─── Unix + xattr feature ─→ xattr.rs (real)
-X flag ──→ cfg ──┤
                   └─── otherwise ────────────→ xattr_stub.rs (warning)

                   ┌─── Unix + xattr feature ─→ nfsv4_acl.rs (real)
NFSv4 ACL ─→ cfg ─┤
                   └─── otherwise ────────────→ nfsv4_acl_stub.rs (no-op)
```
