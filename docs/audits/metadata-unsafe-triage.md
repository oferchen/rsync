# metadata unsafe triage for the two-owner migration

Measured 2026-09-15 on master `adacbe888`. Scope: production code in
`crates/metadata/src/` - `#[cfg(test)]` items and test-only modules are
excluded at item granularity.

## Instrument and validation

The census is a lexer-level scan: comments, string literals, and
`unsafe_code` attribute tokens are stripped; `#[cfg(test)]` /
`#[cfg(all(test, ...))]` items are skipped by tracking brace depth from the
attribute, and files reachable only through a `#[cfg(test)] mod` declaration
(including `acl_windows/tests/`, `mapping/tests.rs`, `chmod/tests.rs`) are
excluded whole. Each counted occurrence is one `unsafe { ... }` block or one
`unsafe extern` declaration.

Validation against known anchors before trusting the metadata numbers:

| crate | measured | expected anchor | verdict |
|---|---|---|---|
| `flist` | 8 | 8 (ledger 2026-08-28; the statx-to-fast_io move is on a side branch, not yet in this base) | exact match |
| `fast_io` | 375 | "hundreds" (ledger said 433 on 2026-08-28; the count has drifted down since) | consistent |
| `metadata` | **80** | ledger said ~96 on 2026-08-28 | drifted down 16 |

The fast_io pure-safe `statx.rs` control does not exist in this base (its
branch is unmerged), so the flist count itself serves as the known-ground
anchor; it matches the ledger exactly.

## Totals

**80 production unsafe blocks** in `crates/metadata/src/`, classified:

| class | blocks |
|---|---|
| (a) RELOCATE-TO-FAST_IO | 39 |
| (b) RELOCATE-TO-PLATFORM | 30 |
| (c) DELETE-VIA-SAFE-WRAPPER | 11 |
| (d) JUSTIFIED-STAY | 0 |

SAFETY comment quality: 78 of 80 blocks carry a substantive tagged `SAFETY:`
comment (some cover a tightly-coupled pair, e.g. the umask read/restore).
The two `xattr_unix.rs` blocks have explanatory prose but no tagged
`SAFETY:` line - the only gap.

Wrapper versions verified in `Cargo.lock` and the registry sources:
`nix 0.31.3` (metadata already depends on it, features `user`, `fs`),
`rustix 1.1.4` (features `fs`, `process`, default `alloc`),
`exacl 0.13.0`, `windows 0.62.2`, `xattr 1.6.1`. Toolchain 1.88.0.

## Class (c) - DELETE-VIA-SAFE-WRAPPER (11 blocks)

Tried first per policy. Every wrapper below was verified to exist at the
pinned version by reading the registry source, including its cfg gates.

| site | call | SAFETY | wrapper (verified) | rationale |
|---|---|---|---|---|
| `identity.rs:57` | `libc::geteuid` | present | `nix::unistd::geteuid` (unistd.rs:1770) | nix wraps the libc symbol, so fakeroot interposition is preserved - the in-file requirement ("libc, not rustix") holds; the `am_root_tracks_the_libc_effective_uid` test guards it |
| `identity.rs:64` | `libc::getegid` | present | `nix::unistd::getegid` (unistd.rs:1790) | same |
| `apply/ownership.rs:235` | `libc::getgroups(0, NULL)` size probe | present | `rustix::process::getgroups` (process/id.rs:252, `alloc` on by default) | one safe call replaces the probe+fill pair; available on Apple (nix gates its own `getgroups` off Apple, which is why libc was used) |
| `apply/ownership.rs:241` | `libc::getgroups(n, buf)` | present | same | deleted together with the probe |
| `apply/permissions.rs:94` | `libc::umask(0)` read | present (pair) | `nix::sys::stat::umask` (sys/stat.rs:209) | symbol-backed safe wrapper; same set-then-restore pattern |
| `apply/permissions.rs:95` | `libc::umask(old)` restore | present (pair) | same | deleted with its pair |
| `chmod/parse.rs:56` | `libc::umask(0)` read | present (pair) | `nix::sys::stat::umask` | duplicate of the `apply/permissions.rs` OnceLock pattern - consolidate to one owner while deleting |
| `chmod/parse.rs:57` | `libc::umask(old)` restore | present (pair) | same | deleted with its pair |
| `apply/timestamps.rs:785` | `CloseHandle` (RAII drop) | present | std - handle ownership moves into `std::fs::File` | disappears with the two blocks below |
| `apply/timestamps.rs:801` | `CreateFileW` (`FILE_WRITE_ATTRIBUTES`, `FILE_FLAG_BACKUP_SEMANTICS`) | present | `std::fs::OpenOptions` + `std::os::windows::fs::OpenOptionsExt::{access_mode, custom_flags}` | opens files and directories identically to the raw call |
| `apply/timestamps.rs:819` | `SetFileTime` (creation time) | present | `std::fs::File::set_times` + `std::os::windows::fs::FileTimesExt::set_created` (stable since 1.75; toolchain is 1.88) | creation-time set is expressible in pure std |

Caveat recorded for `ownership.rs`: on Linux, rustix issues the raw
`getgroups` syscall rather than the libc symbol. `getgroups` is not part of
the identity set fakeroot fakes for our tests (only the euid/egid reads in
`identity.rs` carry that requirement), but if symbol-backing is wanted here
too, the alternative is class (b) via a platform API; see decisions below.

## Class (b) - RELOCATE-TO-PLATFORM (30 blocks)

Process, identity, and account-database FFI. None is expressible with a
safe wrapper at the pinned versions (each rejection is stated).

| site | call | SAFETY | rationale |
|---|---|---|---|
| `copy_as.rs:152` | `libc::setgroups` (Apple fallback) | present | nix 0.31.3 gates `setgroups` off `apple_targets` (unistd.rs:1954); `platform::privilege` already owns the identical Apple fallback (privilege.rs:157) - consume `platform::privilege::set_supplementary_groups` and delete this copy |
| `copy_as.rs:175` | `libc::initgroups` (Apple fallback) | present | nix gates `initgroups` off `apple_targets` (unistd.rs:2089); belongs beside platform's setgroups fallback |
| `copy_as.rs:332` | `CloseHandle` (token RAII drop) | present | process-token privilege probe - platform::privilege charter |
| `copy_as.rs:343` | `OpenProcessToken` | present | same probe |
| `copy_as.rs:354` | `LookupPrivilegeValueW` | present | same probe |
| `copy_as.rs:371` | `PrivilegeCheck` | present | same probe |
| `netgroup.rs:48` | `unsafe extern "C" { fn innetgr }` | n/a (decl; upstream ref present) | the `libc` crate does not export `innetgr`, and neither nix nor rustix wraps it - host access-control identity, platform charter |
| `netgroup.rs:68` | `innetgr(...)` call | present | moves with its declaration |
| `id_lookup/nss.rs:41` | `libc::getpwuid_r` | present | (c) REJECTED: `nix::unistd::User::from_uid` exists (unistd.rs:3763) but converts `pw_name` with `to_string_lossy()` (unistd.rs:3555), silently mangling non-UTF-8 account names that the current code and upstream (`uidlist.c`) keep byte-exact; `platform::group` already owns this `*_r` FFI family |
| `id_lookup/nss.rs:56` | `assume_init` after `getpwuid_r` | present | moves with its call |
| `id_lookup/nss.rs:58` | `CStr::from_ptr(pw_name)` | present | moves with its call |
| `id_lookup/nss.rs:105` | `libc::getpwnam_r` | present | same lossy-name rejection of `nix::unistd::User::from_name` (unistd.rs:3787) |
| `id_lookup/nss.rs:120` | `assume_init` after `getpwnam_r` | present | moves with its call |
| `id_lookup/nss.rs:154` | `libc::getgrgid_r` | present | same rejection of `nix::unistd::Group::from_gid` (unistd.rs:3924, lossy at unistd.rs:3824) |
| `id_lookup/nss.rs:169` | `assume_init` after `getgrgid_r` | present | moves with its call |
| `id_lookup/nss.rs:171` | `CStr::from_ptr(gr_name)` | present | moves with its call |
| `id_lookup/nss.rs:218` | `libc::getgrnam_r` | present | same rejection of `nix::unistd::Group::from_name` (unistd.rs:3949); `platform::group` wraps `getgrnam_r` today |
| `id_lookup/nss.rs:233` | `assume_init` after `getgrnam_r` | present | moves with its call |
| `id_lookup/nss.rs:331` | `libc::getgrouplist` (Apple variant) | present | nix gates `getgrouplist` off Apple; rustix has none - identity lookup, platform charter |
| `id_lookup/nss_win.rs:46` | `NetUserGetLocalGroups` | present | account database; sibling of platform's `NetLocalGroupGetMembers` in `group.rs` |
| `id_lookup/nss_win.rs:73` | `slice::from_raw_parts` over NetApi buffer | present | moves with its call |
| `id_lookup/nss_win.rs:77` | `PWSTR::to_string` | present | moves with its call |
| `id_lookup/nss_win.rs:84` | `NetApiBufferFree` | present | moves with its call |
| `acl_windows/dacl.rs:85` | `IsValidSid` | present | SID inspection - platform::name_resolution already owns SID sub-authority FFI |
| `acl_windows/dacl.rs:96` | `LookupAccountSidW` (size probe) | present | duplicates platform::name_resolution's account/SID resolution |
| `acl_windows/dacl.rs:115` | `LookupAccountSidW` (fill) | present | same |
| `acl_windows/dacl.rs:131` | `GetSidSubAuthorityCount` | present | platform::name_resolution wraps this exact call today |
| `acl_windows/dacl.rs:136` | `GetSidSubAuthority` | present | same |
| `acl_windows/dacl.rs:603` | `LookupAccountNameW` (size probe) | present | duplicates platform::name_resolution `LookupAccountName` |
| `acl_windows/dacl.rs:621` | `LookupAccountNameW` (fill) | present | same |

## Class (a) - RELOCATE-TO-FAST_IO (39 blocks)

Filesystem/I-O syscalls: xattr reads, alternate-data-stream I/O, file
security-descriptor get/set, reparse-point ioctl, disk-size ioctls,
`setattrlist`.

| site | call | SAFETY | rationale |
|---|---|---|---|
| `device_size.rs:72` | `libc::ioctl` DKIOCGETBLOCKSIZE + DKIOCGETBLOCKCOUNT | present | disk ioctls; fast_io already owns the reflink ioctls - nix's `ioctl_read!` only generates more unsafe, so no (c) |
| `xattr_unix.rs:88` | `libc::getxattr` (size probe, Apple `position` arg) | **absent** (untagged prose only) | Apple resource-fork chunked read; the `xattr` crate (1.6.1) and rustix expose no `position`/chunk API, so no (c) |
| `xattr_unix.rs:113` | `libc::getxattr` (chunked fill) | **absent** | same |
| `xattr_windows.rs:156` | `FindClose` (RAII drop) | present | ADS-as-xattr I/O cluster |
| `xattr_windows.rs:179` | `GetLastError` | present | trivial accessor, moves with the cluster |
| `xattr_windows.rs:207` | `mem::zeroed::<WIN32_FIND_STREAM_DATA>` | present | POD init for the stream walk |
| `xattr_windows.rs:212` | `FindFirstStreamW` | present | stream enumeration |
| `xattr_windows.rs:229` | `GetLastError` | present | moves with the cluster |
| `xattr_windows.rs:246` | `FindNextStreamW` | present | stream enumeration |
| `xattr_windows.rs:249` | `GetLastError` | present | moves with the cluster |
| `xattr_windows.rs:277` | `CreateFileW` (stream read open) | present | `File::open` cannot name a `:stream:$DATA` path portably with the exact share/disposition flags; fast_io owns `CreateFileW` usage already |
| `xattr_windows.rs:292` | `File::from_raw_handle` | present | handle handoff, moves with its open |
| `xattr_windows.rs:321` | `CreateFileW` (stream write open) | present | same |
| `xattr_windows.rs:336` | `File::from_raw_handle` | present | same |
| `xattr_windows.rs:358` | `DeleteFileW` (stream delete) | present | stream I/O |
| `acl_windows/common.rs:161` | `LocalFree` (security-descriptor RAII) | present | frees buffers returned by the Get/SetNamedSecurityInfoW cluster |
| `acl_windows/common.rs:185` | `LocalFree` (SDDL string RAII) | present | same |
| `acl_windows/dacl.rs:50` | `GetNamedSecurityInfoW` | present | file security metadata read - "ACL syscalls" are named in the fast_io relocation charter |
| `acl_windows/dacl.rs:191` | `GetAclInformation` | present | parses the fetched file DACL |
| `acl_windows/dacl.rs:206` | `GetAce` | present | same |
| `acl_windows/dacl.rs:214` | `&*(ace_ptr as *const ACE_HEADER)` | present | same |
| `acl_windows/dacl.rs:221` | `&*(ace_ptr as *const ACCESS_ALLOWED_ACE)` | present | same |
| `acl_windows/dacl.rs:533` | `InitializeAcl` | present | builds the DACL written back to the file |
| `acl_windows/dacl.rs:549` | `AddAccessAllowedAce` | present | same |
| `acl_windows/dacl.rs:568` | `SetNamedSecurityInfoW` | present | file security metadata write |
| `acl_windows/sddl.rs:94` | `GetNamedSecurityInfoW` | present | file security read |
| `acl_windows/sddl.rs:119` | `ConvertSecurityDescriptorToStringSecurityDescriptorW` | present | serializes the fetched SD; exists only to service the file read/write pair |
| `acl_windows/sddl.rs:143` | `slice::from_raw_parts` over SDDL PWSTR | present | same |
| `acl_windows/sddl.rs:177` | `ConvertStringSecurityDescriptorToSecurityDescriptorW` | present | same, write direction |
| `acl_windows/sddl.rs:203` | `GetSecurityDescriptorOwner` | present | SD field accessors on the fetched buffer |
| `acl_windows/sddl.rs:211` | `GetSecurityDescriptorGroup` | present | same |
| `acl_windows/sddl.rs:220` | `GetSecurityDescriptorDacl` | present | same |
| `acl_windows/sddl.rs:229` | `GetSecurityDescriptorSacl` | present | same |
| `acl_windows/sddl.rs:273` | `SetNamedSecurityInfoW` | present | file security write |
| `apply/timestamps.rs:701` | `mem::zeroed::<libc::attrlist>` | present | POD init for the call below |
| `apply/timestamps.rs:717` | `libc::setattrlist` (crtime, `FSOPT_NOFOLLOW`) | present (upstream ref: `syscall.c:do_setattrlist_crtime`) | `setattrlist` is named in the fast_io relocation charter; no nix/rustix wrapper exists |
| `windows/reparse.rs:208` | `DeviceIoControl` FSCTL_GET_REPARSE_POINT | present | reparse ioctl - fast_io's ioctl charter |
| `windows/reparse.rs:282` | `CloseHandle` (RAII drop) | present | moves with its open |
| `windows/reparse.rs:314` | `CreateFileW` (`FILE_FLAG_OPEN_REPARSE_POINT`) | present | open-without-follow; fast_io owns `CreateFileW` and `to_extended_path` already |

## Class (d) - JUSTIFIED-STAY

None. Every block either has a verified safe replacement or falls squarely
inside one owner's charter.

## Recommended migration order

Smallest independent clusters first, deletions before relocations
(the flist statx precedent):

1. **umask consolidation (4 blocks, one PR):** `apply/permissions.rs:94-95`
   + `chmod/parse.rs:56-57` onto `nix::sys::stat::umask`, merging the two
   duplicated OnceLock patterns into one owner.
2. **identity euid/egid (2 blocks):** `identity.rs:57,64` onto
   `nix::unistd::geteuid`/`getegid`; the existing fakeroot-parity test is
   the regression guard.
3. **Windows creation-time (3 blocks):** `apply/timestamps.rs:785-819` onto
   std `OpenOptionsExt` + `FileTimesExt::set_created`.
4. **getgroups (2 blocks):** `apply/ownership.rs:235,241` onto
   `rustix::process::getgroups` (pending decision 3 below).
5. **copy_as Apple group fallbacks (2 blocks):** delete by consuming
   `platform::privilege::set_supplementary_groups`; add an initgroups
   sibling to platform.
6. **copy_as Windows privilege probe (4 blocks)** to platform::privilege.
7. **netgroup innetgr (2 blocks)** to platform.
8. **nss_win (4 blocks)** to platform::group.
9. **nss.rs `*_r` family (11 blocks)** to platform (byte-exact port; do NOT
   detour through nix's lossy `User`/`Group`).
10. **Apple I/O cluster (5 blocks):** `device_size.rs`, `xattr_unix.rs`
    (add the missing SAFETY tags in the move), `setattrlist` to fast_io.
11. **Windows reparse (3 blocks)** to fast_io.
12. **Windows ADS xattr (12 blocks)** to fast_io.
13. **Windows ACL/SDDL (26 blocks)** last - largest and gated on decision 1.

## Decisions needed

1. **acl_windows split.** The classification sends 19 blocks to fast_io
   (file SD get/set, ACE/DACL marshalling, SDDL conversion) and 7 to
   platform (SID/account resolution that duplicates
   `platform::name_resolution`). That splits one cohesive module across
   both owners. Alternative: move the whole module to fast_io under its
   "ACL syscalls" charter and have it call platform::name_resolution's
   safe lookup API for the 7 resolution sites. A call is needed before
   step 13.
2. **nss.rs lossy-name rejection.** `nix::unistd::User`/`Group` wrappers
   exist and would delete 10 blocks, but their `to_string_lossy()` name
   conversion breaks byte-exact non-UTF-8 account names (upstream keeps
   bytes). Classified (b) on fidelity grounds; confirm that non-UTF-8
   account names are in scope (they are for upstream parity).
3. **getgroups backing.** `rustix::process::getgroups` on Linux issues the
   raw syscall, not the libc symbol, so any libc interposer (fakeroot)
   would be bypassed for this one call. No current test depends on faked
   getgroups; if symbol-backing is required anyway, reroute through a
   platform API (class (b)) instead.
