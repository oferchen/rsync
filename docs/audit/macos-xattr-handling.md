# macOS-specific xattr handling audit (XAP-8)

Tracks parent XAP series (`[[project_xattr_acl_cross_platform_parity_gap]]`)
and feeds the direction-matrix synthesis in XAP-9.

## 1. Scope

This audit compares oc-rsync's macOS extended-attribute behaviour to upstream
rsync 3.4.1 with `-X` (`--xattrs`) on macOS. Coverage:

- Namespace filtering for the `com.apple.*` family.
- Special-case xattrs: `com.apple.ResourceFork`, `com.apple.FinderInfo`,
  `com.apple.quarantine`, `com.apple.metadata:*`.
- Cross-platform delivery (macOS source -> Linux/Windows destinations and
  vice versa).
- Deviations of oc-rsync from upstream's macOS code path.

Out of scope: AppleDouble (`._foo`) sidecar handling (covered by
`docs/audits/apple-fs-roundtrip.md` Medium-1, and the filter chain's
`apple_double_default_patterns()`), `clonefile(2)` fast-path xattr inheritance
(covered by `crates/engine/src/local_copy/clonefile.rs`), and HFS+ -> APFS
filename NFD/NFC normalisation (`apple_fs::normalize_filename`).

## 2. Background

macOS exposes one flat xattr namespace - there is no `user.` / `system.` /
`security.` / `trusted.` partition that Linux enforces in the kernel. Every
name is a free-form byte string subject only to the filesystem's per-attribute
size limit. `man 2 getxattr` documents the API; Apple convention reserves the
`com.apple.*` prefix for system-managed metadata, with the most visible
slots being:

- `com.apple.ResourceFork` - legacy resource fork. Stored as an ordinary
  xattr but exposed by older Carbon APIs through the `/..namedfork/rsrc`
  pseudo-path. Payloads can run to multiple MB for legacy media files.
- `com.apple.FinderInfo` - fixed-size 32-byte struct holding file type /
  creator codes and Finder flags. The kernel rejects non-32-byte payloads
  with `EINVAL`.
- `com.apple.quarantine` - Gatekeeper / Safari download provenance flag.
  Triggers the "this file was downloaded from the internet" dialog on first
  launch.
- `com.apple.metadata:_kMDItemUserTags` - bplist holding Finder colour tags
  ("Red", "Green", ...).
- `com.apple.metadata:kMDItemWhereFroms` - bplist with the originating URL.
- `com.apple.lastuseddate#PS` - Spotlight's last-used tracking.
- `com.apple.diskimages.fsck` / `com.apple.diskimages.recentcksum` - DMG
  scrub bookkeeping.

The macOS syscall surface (`getxattr(2)`, `setxattr(2)`, `listxattr(2)`,
`removexattr(2)`) takes a `position` argument that is meaningful only for the
resource fork and an `options` flag carrying `XATTR_NOFOLLOW` and
`XATTR_NOSECURITY`. The `xattr` crate hard-codes `position = 0` and folds
`follow_symlinks` into the `options` argument.

## 3. Inventory of macOS xattr handling in oc-rsync

The xattr stack has zero `com.apple.*` interception in the production paths.
The only places that name macOS xattrs explicitly are the `apple-fs`
convenience accessors (not on the transfer hot path) and one assertion in the
non-Linux unit tests.

### 3.1 Cross-platform xattr facade

`crates/metadata/src/xattr.rs`:

- Line 39-41: rustdoc comment notes that "On non-Linux platforms (macOS,
  FreeBSD, Windows): no namespace filtering, since those systems use a single
  flat namespace (NTFS ADS, `com.apple.*`, FreeBSD `user`-only, etc.)".
- Lines 64-67: `is_xattr_permitted` returns `true` unconditionally on every
  non-Linux target. No `com.apple.*` allow-list, deny-list, or any other name
  inspection runs on macOS.
- Lines 75-83: `list_attributes` calls the platform backend, byte-maps each
  name via `os_name_to_bytes`, then runs every name through
  `is_xattr_permitted`. On macOS the filter is the identity function, so the
  raw `listxattr(2)` output is forwarded unchanged.
- Lines 121-157: `read_xattrs_for_wire` translates names with
  `protocol::xattr::local_to_wire` (see 3.3), reads each value with
  `read_attribute`, sorts, and assigns reverse 1-based nums. No macOS-specific
  branch.
- Lines 222-279: `apply_xattrs_from_list` writes every entry whose name passes
  `is_xattr_permitted` (always true on macOS) and removes destination xattrs
  not in the source list. The receiver-side path never special-cases
  `com.apple.FinderInfo` length or `com.apple.ResourceFork` size.
- Line 285: `RESERVED_SDDL_XATTR = b"user.win32.security_descriptor"` -
  Windows-specific slot, skipped on every other platform including macOS.
- Lines 552-558: `is_xattr_permitted_allows_all_on_non_linux` test asserts
  that `com.apple.quarantine` is permitted on non-Linux (the only test in
  this file that names a `com.apple.*` attribute).

### 3.2 Unix xattr backend

`crates/metadata/src/xattr_unix.rs`:

- Lines 20-27: `list_attributes` calls `xattr::list` /`xattr::list_deref`.
  On macOS the `xattr` crate (v1.6) invokes `listxattr(2)` with
  `XATTR_NOFOLLOW` toggled by the `follow_symlinks` argument.
- Lines 30-41: `read_attribute` calls `xattr::get` /`xattr::get_deref`,
  hardcoded to `position = 0`.
- Lines 44-56: `write_attribute` calls `xattr::set` /`xattr::set_deref`,
  also `position = 0`. The macOS kernel's 32-byte enforcement for
  `com.apple.FinderInfo` surfaces as an `io::Error` from this call - the
  upstream protocol does not pre-validate length, and neither does the Rust
  wrapper.
- Lines 61-68: `remove_attribute` calls `xattr::remove`. `ENOATTR` is mapped
  to an `Err` here; the higher layer (`xattr.rs`) lists first and only
  removes existing names, so missing-name failures do not surface in
  practice.
- Line 74-76: `os_name_to_bytes` is a zero-copy `OsStr -> Vec<u8>` view via
  `OsStrExt::as_bytes()`. Both `com.apple.ResourceFork` and arbitrary
  UTF-8 names round-trip unchanged.

### 3.3 Wire-format prefix translator

`crates/protocol/src/xattr/prefix.rs`:

- Lines 80-83: Linux sender ships the local name verbatim
  (`local_to_wire(name, am_root) -> Some(name.to_vec())`).
- Lines 85-97: non-Linux (macOS, FreeBSD) sender adds the `user.` prefix to
  every name that is not already disguised under `RSYNC_PREFIX`. Therefore
  the wire bytes for a macOS source's `com.apple.ResourceFork` are
  `user.com.apple.ResourceFork`. This mirrors upstream `xattrs.c:518-530`.
- Lines 107: `USER_PREFIX_NON_LINUX = "user."`.
- Lines 140-161 (Linux receiver branch): `user.*` names land verbatim;
  non-user names land verbatim for root and are disguised under
  `user.rsync.<wire_name>` for non-root receivers.
- Lines 163-188 (non-Linux receiver branch): `user.*` is stripped (so
  `user.com.apple.ResourceFork` re-emerges as `com.apple.ResourceFork`).
  Non-user-namespace wire names are dropped for non-root receivers (return
  `None`) and disguised under `rsync.<wire_name>` for root receivers.
- Lines 194-204: `is_rsync_internal` recognises `user.rsync.%suffix` and
  `rsync.%suffix` so internal metadata is never sent.

### 3.4 Resource fork / Finder info convenience accessors

`crates/apple-fs/src/resource_fork.rs` (not on the transfer hot path; used
only by the AppleDouble round-trip helpers and by callers that want a typed
API):

- Lines 26-29: `RESOURCE_FORK_XATTR = "com.apple.ResourceFork"`,
  `FINDER_INFO_XATTR = "com.apple.FinderInfo"`, `FINDER_INFO_LEN = 32`.
- Lines 45-69: `read_resource_fork`, `write_resource_fork`,
  `remove_resource_fork` - thin `xattr::{get,set,remove}` wrappers on macOS,
  no-op stubs everywhere else.
- Lines 82-119: `read_finder_info` / `write_finder_info` /
  `remove_finder_info`. The reader enforces the 32-byte invariant
  (`io::ErrorKind::InvalidData` for anything else); the writer takes a
  `&[u8; FINDER_INFO_LEN]`, so the type system rules out wrong-size writes
  at the call site.
- Lines 147-155: `is_no_attr` checks `raw_os_error() == Some(93)` for
  Darwin's `ENOATTR`. Used to make `remove_resource_fork` /
  `remove_finder_info` idempotent.

These accessors are wired into `crates/apple-fs/src/apple_double.rs` and the
AppleDouble round-trip integration test (`crates/apple-fs/tests/
apple_double_round_trip.rs`). Nothing in the sender / receiver hot path calls
them directly - the `com.apple.ResourceFork` and `com.apple.FinderInfo`
attributes flow through the generic xattr pipeline described in 3.1-3.3, the
same way upstream rsync handles them.

### 3.5 `fast_io` and `engine`

A grep across `crates/fast_io/src/` and `crates/engine/src/` for
`com.apple`, `ResourceFork`, `FinderInfo`, or `quarantine` returns zero hits.
The platform copy fast paths (`fast_io::macos_io`, `clonefile`) only ever
touch the file's primary data fork; xattrs round-trip via the cross-platform
xattr layer.

## 4. Upstream rsync's macOS path

Source tree: `target/interop/upstream-src/rsync-3.4.1/`.

### 4.1 Backend selection

`lib/sysxattrs.c:27-100`:

- Line 27-29: `#ifdef HAVE_OSX_XATTRS / #define GETXATTR_FETCH_LIMIT
  (64*1024*1024)` - the only macOS-specific tuning constant in the entire
  upstream xattr code path.
- Lines 58-100: macOS backend (`#elif HAVE_OSX_XATTRS`). The four
  `sys_l{get,set,remove,list}xattr` wrappers call
  `getxattr/setxattr/removexattr/listxattr` with `XATTR_NOFOLLOW`.
- Lines 60-80: `sys_lgetxattr` contains a chunked-read loop. When the first
  `getxattr` returns exactly `GETXATTR_FETCH_LIMIT` (64 MiB) AND the caller
  asked for more, it re-enters `getxattr` with a non-zero `position` and
  appends until the buffer is filled. This is upstream's defence against
  macOS's hard 64 MiB cap on a single `getxattr` call for resource forks.

### 4.2 Namespace policy in `xattrs.c`

`target/interop/upstream-src/rsync-3.4.1/xattrs.c`:

- Lines 59-71: `USER_PREFIX`, `SYSTEM_PREFIX`, `RSYNC_PREFIX`. On Linux
  `RSYNC_PREFIX = "user.rsync."`; everywhere else (macOS included)
  `RSYNC_PREFIX = "rsync."`. `MIGHT_NEED_RPRE` is gated by `am_root` on
  non-Linux and by `am_root <= 0` on Linux.
- Lines 254-258: `rsync_xal_get` skips `system.*` (root) or any non-`user.*`
  (non-root) attribute - but **only inside `#ifdef HAVE_LINUX_XATTRS`**. The
  macOS build path does not run this filter. Every xattr name returned by
  `listxattr(2)` is queued for transmission.
- Lines 260-268: rsync-internal `rsync.%FOO` slots are skipped on send. On
  macOS these are `rsync.%stat`, `rsync.%aacl`, `rsync.%dacl` (no `user.`
  prefix).
- Lines 358-360 (`copy_xattrs`): same Linux-only namespace skip.
- Lines 509-532 (`send_xattr`): the macOS sender enters the
  `#ifndef HAVE_LINUX_XATTRS` arm. It writes the wire-encoded name as
  `USER_PREFIX + local_name` (i.e. `user.com.apple.ResourceFork`) so the
  receiver sees the same byte sequence it would have produced from a Linux
  source's `user.com.apple.ResourceFork`.
- Lines 818-847 (`receive_xattr`): the macOS receiver enters the `#else`
  branch (line 832). Wire names with `user.` are stripped of that prefix
  (becoming `com.apple.ResourceFork` again); names without `user.` are
  disguised under `rsync.<wire_name>` if `am_root`, dropped otherwise.
- Lines 848-853: rsync-internal `rsync.%FOO` slots are dropped on receive
  unless `preserve_xattrs >= 2` (two `-X` flags).

### 4.3 Special-case handling - none

There is no special-case code for `com.apple.ResourceFork`,
`com.apple.FinderInfo`, `com.apple.quarantine`, or any other Apple name in
`xattrs.c`. Upstream treats them as opaque byte payloads and relies on the
kernel for size enforcement. The only macOS-specific tuning anywhere in the
xattr stack is the 64 MiB chunked-read loop in `lib/sysxattrs.c:60-80`.

### 4.4 ACL adjacent paths (out of scope for XAP-8)

`acls.c` carries five `HAVE_OSX_ACLS` blocks (lines 102, 283, 362, 390, 407,
764, 856, 984) handling the macOS ACL surface via `lib/sysacls.c:2601` and
`lib/sysacls.c:2780`. These do not interact with the xattr path. ACL
direction-matrix work belongs to XAP-2/4.

## 5. Direction matrix for macOS xattrs

Default flags assumed: `oc-rsync -aAX` (or the equivalent upstream invocation).
"Verbatim" means the named bytes round-trip byte-for-byte through both ends'
filesystems.

| Direction | Default behaviour | Risk |
|-----------|-------------------|------|
| macOS source -> macOS dest | All `com.apple.*` xattrs round-trip verbatim. Wire form prepends `user.`; receiver strips it. Resource fork chunking happens entirely inside the `xattr` crate's `getxattr` call. | `com.apple.quarantine` survives, attaching the source's Gatekeeper state to the destination copy (footgun for backups). Resource forks > 64 MiB are read in a single `getxattr(2)` by oc-rsync - upstream loops, oc-rsync does not (see Finding F2). |
| macOS source -> Linux dest | Names enter Linux as `user.com.apple.*` (the `user.` prefix is from the wire). Bytes preserved; Linux consumers cannot interpret the payload. | Semantic loss only - Linux has no Finder, no Gatekeeper, no resource-fork-aware tooling. Linux's per-attribute byte cap (xfs ~64 KiB, ext4 ~4 KiB inline) may reject large resource forks with `ENOSPC`; oc-rsync surfaces the error verbatim. |
| macOS source -> Windows dest | Routed through `crates/metadata/src/xattr_windows.rs`. Each xattr maps to an NTFS Alternate Data Stream named `path:<name>:$DATA`. `com.apple.ResourceFork` would land as the ADS `path:com.apple.ResourceFork:$DATA` - which `stream_path_wide` accepts because the colon parser treats only the first `:` as the stream separator. | ADS stream names with embedded `:` are reserved Win32 syntax; behaviour on non-NTFS volumes (FAT, exFAT) collapses to a hard `ERROR_INVALID_PARAMETER`. See WPC-1 audit (`docs/audit/windows-ads-handling.md`). |
| Linux source -> macOS dest | `user.foo` (from the wire) gets the `user.` prefix stripped by the macOS receiver, landing as bare `foo`. `user.rsync.*` disguise survives intact. | OK. Round-trip with Linux is symmetric. |
| Windows source -> macOS dest | ADS stream names ship as bare bytes; macOS receiver strips the wire's `user.` prefix and writes them through the flat namespace. | OK. macOS will accept arbitrary names but does not interpret a stream named `com.apple.ResourceFork` as a resource fork unless the upstream Windows transfer happened to ride exactly that name. |
| macOS dest receiving from Linux/Windows sender that previously round-tripped Apple content | Symmetric to the above. Names like `user.com.apple.ResourceFork` on the Linux sender re-emerge as `com.apple.ResourceFork` on macOS. | Re-roundtrip through Windows requires the ADS name to be exactly `com.apple.ResourceFork`. Upstream tooling does not preserve the colon in `com.apple.metadata:_kMDItemUserTags` unambiguously - this is a known Windows-side limitation tracked under WPC-1. |

## 6. Findings

### F1 - No macOS-specific xattr namespace filtering

oc-rsync has no allow-list, deny-list, or rewrite layer for `com.apple.*`
attributes. `is_xattr_permitted` returns `true` unconditionally on every
non-Linux target (`crates/metadata/src/xattr.rs:64-67`). This exactly matches
upstream's behaviour (`xattrs.c:254-258` only filters under
`#ifdef HAVE_LINUX_XATTRS`). No deviation.

### F2 - `com.apple.ResourceFork` 64 MiB chunked-read loop not implemented

Upstream `lib/sysxattrs.c:60-80` loops `getxattr(2)` with rising `position`
arguments when the kernel returns `GETXATTR_FETCH_LIMIT` (64 MiB) on the
first call, allowing arbitrarily large resource forks to be read in
contiguous chunks. The `xattr` crate that oc-rsync depends on
(`crates/metadata/src/xattr_unix.rs:30-41`) hard-codes `position = 0` and
makes one `getxattr` call. On a resource fork larger than 64 MiB the macOS
kernel returns the first 64 MiB and oc-rsync sees a truncated read.

In the wild this is rare - resource forks are a HFS+ legacy feature, and the
only common producers (legacy Carbon media tools, ResEdit-style archives) are
typically under 1 MiB. The 64 MiB boundary is reachable for old film and
audio archives that bundled raw resource data. Filing as a deviation rather
than a defect; tracked for follow-up under XAP-11.

### F3 - `com.apple.quarantine` not filtered (matches upstream, surfaces as a backup-time footgun)

Neither oc-rsync nor upstream rsync filter `com.apple.quarantine`. A backup
sync from a downloaded file to a restored location preserves the quarantine
flag, so Gatekeeper re-flags the file on next launch even though the source
of the byte stream is the user's own backup. Behaviour matches upstream and
is unsurprising once documented; the gap is the absence of an opt-in flag
for the backup use case (see R1).

### F4 - macOS-specific xattr-write errors propagate as opaque `io::Error`

When the macOS kernel rejects an xattr write (`EPERM` on locked attributes,
`EINVAL` for non-32-byte `com.apple.FinderInfo`, `ENOSPC` on a full
filesystem), the error surfaces through
`crates/metadata/src/xattr_unix.rs:44-56` -> `crates/metadata/src/xattr.rs:94-102`
-> `MetadataError` with the wrapping context string
`"write extended attribute"`. There is no Apple-specific error mapping; the
underlying errno is preserved via `MetadataError::source()` so the operator
can still diagnose the root cause. Upstream rsync logs the error via its
generic xattr error path; behaviour matches.

The only asymmetry is that `apple-fs::write_finder_info` enforces the 32-byte
invariant at the Rust type system level (`&[u8; FINDER_INFO_LEN]`), but that
accessor is not on the transfer hot path - the generic `write_attribute`
runs instead and forwards whatever the wire sent.

### F5 - Test coverage for macOS xattrs

In-tree coverage of `com.apple.*` xattrs is:

- `crates/apple-fs/tests/apple_double_round_trip.rs` - AppleDouble container
  -> xattr round trip via the typed `apple-fs` accessors. Targets the
  AppleDouble sidecar path, not the production transfer pipeline.
- `crates/apple-fs/src/resource_fork.rs:253-317` - macOS-gated unit tests
  for the typed accessors (`macos_resource_fork_round_trip`,
  `macos_finder_info_round_trip`, `macos_finder_info_rejects_wrong_length_payload`).
- `crates/metadata/src/xattr.rs:553-558` - `is_xattr_permitted_allows_all_on_non_linux`
  asserts `com.apple.quarantine` passes the namespace check. Single
  assertion; does not exercise read/write/round-trip.
- `tests/acl_xattr_roundtrip_macos.rs` - end-to-end interop test
  (`acl_xattr_roundtrip_macos_oc_then_upstream`,
  `acl_xattr_roundtrip_macos_upstream_then_oc`) that round-trips
  `com.apple.FinderInfo`, `com.apple.metadata:_kMDItemUserTags`, and
  `com.apple.ResourceFork` through oc-rsync and Homebrew rsync in both
  directions. Gated by `OC_RSYNC_METADATA_INTEROP=1`. This is the only test
  that exercises the production transfer pipeline against upstream.

`crates/metadata/tests/` contains no `com.apple.*` references. The gate-env
on `tests/acl_xattr_roundtrip_macos.rs` keeps it from running in default CI
- so the macOS xattr round-trip is exercised only when an operator opts in.
Gap to close under XAP-5 (macOS -> Linux harness).

### F6 - Comparison with upstream behaviour

| Aspect | Upstream rsync 3.4.1 macOS | oc-rsync macOS | Verdict |
|--------|---------------------------|----------------|---------|
| Namespace filter on `com.apple.*` | None | None | match |
| Wire prefix added by sender | `user.` (xattrs.c:518-530) | `user.` (`prefix.rs:85-97`) | match |
| Wire prefix stripped by receiver | strip + disguise non-user under `rsync.*` for root, drop for non-root (xattrs.c:832-847) | identical (`prefix.rs:163-188`) | match |
| Resource fork > 64 MiB chunked read | yes, `lib/sysxattrs.c:60-80` | no (single `getxattr`) | DEVIATION (F2) |
| `com.apple.FinderInfo` 32-byte enforcement | kernel-only | kernel-only on hot path; typed `apple-fs` accessor adds Rust-side enforcement off the hot path | match in practice |
| `com.apple.quarantine` filtering | none | none | match |
| `XATTR_NOFOLLOW` plumbed through `follow_symlinks` | yes | yes (`xattr_unix.rs:20-67`) | match |
| `rsync.%FOO` internal slot suppression | both ends (xattrs.c:260-267, 848-853) | both ends (`prefix.rs:67-70`, `is_rsync_internal` in `prefix.rs:194-204`, `apply_xattrs_from_list` skip via `is_reserved_sddl_xattr`) | match (modulo Windows SDDL slot) |

Net: one deviation (F2), and one footgun shared with upstream (F3).

## 7. Risk surface

Concrete user-visible risks at the macOS xattr layer:

### 7.1 macOS source -> Linux destination

`com.apple.metadata:_kMDItemUserTags` (Finder colour tags) and
`com.apple.metadata:kMDItemWhereFroms` (download origin URL) are stored
verbatim on Linux as `user.com.apple.metadata:_kMDItemUserTags` etc. The
bytes round-trip cleanly back to macOS - so a Linux-staged backup of a
tagged file preserves the tags through a subsequent Linux -> macOS restore.
No semantic loss in either direction; only the intermediate Linux tooling
cannot interpret the payload (which it never could).

### 7.2 macOS source -> macOS destination (Gatekeeper footgun)

`com.apple.quarantine` survives the transfer. A backup-restored file is
re-quarantined at next launch even though it originated from the user's own
backup. Mirrors upstream and is documented as a known characteristic in
section 8 (R1).

### 7.3 macOS source -> macOS destination (resource fork size)

`com.apple.ResourceFork` payloads above 64 MiB are read in a single
`getxattr(2)` call by oc-rsync. macOS returns up to 64 MiB and the rest is
silently truncated. Upstream rsync loops to recover the full payload (see
F2). The threshold is high enough that the failure mode is reserved for
multi-MB legacy media files; common Carbon media is below 1 MiB. Tracked as
a deviation under XAP-11.

### 7.4 macOS source -> Windows destination

Names containing `:` (notably `com.apple.metadata:_kMDItemUserTags`) collide
with the Win32 ADS stream-name separator. `stream_path_wide`
(`crates/metadata/src/xattr_windows.rs`) builds the stream path by literal
string concatenation, so the receiver opens
`<file>:com.apple.metadata:_kMDItemUserTags:$DATA` which Win32 parses as the
stream `com.apple.metadata` of type `_kMDItemUserTags`. Result: the actual
write target name is mangled. This is a Windows-side gap (WPC-1 audit
F-medium-1 covers the related ADS namespace question) and propagates here
because macOS xattrs are the most common producer of colon-bearing names.

### 7.5 Non-root macOS sender on a `com.apple.*` attribute it cannot read

Some `com.apple.*` attributes (e.g., `com.apple.metadata:_kTimeMachineFoo`)
return `EPERM` for non-root readers. The error surfaces as
`MetadataError("read extended attribute", path, EPERM)` and aborts the
file's xattr transfer. Upstream behaviour is identical; both implementations
inherit it from the kernel.

## 8. Recommendations

### R1 - Opt-in `--macos-strip-quarantine` flag (XAP-11 follow-up)

Add a Receiver-side flag that drops `com.apple.quarantine` from the xattr
stream during apply. Default OFF (upstream-compatible). Document in the
backup-recipe section of the user docs. This is a strict superset of the
existing behaviour and does not affect the wire format. File as a follow-up
under the XAP-11 backlog.

### R2 - Document macOS xattr round-trip semantics

Cross-platform xattr semantics deserve a user-facing note alongside the
direction-matrix spec from XAP-1. XAP-10 owns the user docs deliverable
(`docs/user/xattr-acl-cross-platform.md` or similar). Should call out:

- `com.apple.*` survives a Linux-staged backup verbatim.
- Quarantine survival across backups.
- 64 MiB resource fork ceiling until F2 is closed.
- Colon-in-name collision with Windows ADS.

### R3 - Add macOS xattr round-trip regression coverage to XAP-5

The XAP-5 (macOS -> Linux) harness should include at minimum:

- `com.apple.FinderInfo` (32-byte fixed-size).
- `com.apple.quarantine` (small string).
- `com.apple.metadata:_kMDItemUserTags` (bplist; tests colon handling).
- A `com.apple.ResourceFork` payload (a small one, since the 64 MiB regime is
  tracked separately as F2).

`tests/acl_xattr_roundtrip_macos.rs` is the template; promote its
fixture-build helper into the XAP-3 harness primitive when it lands.

### R4 - macOS support notes for the resource-fork performance characteristic

Add a section to the macOS-specific user notes (sibling of the planned
Windows support matrix from WPC-13) calling out:

- 64 MiB single-`getxattr` ceiling for `com.apple.ResourceFork` (F2).
- `apple-fs` typed accessors are not on the hot path; the generic xattr
  pipeline is the one that runs during `oc-rsync -X`.
- `com.apple.FinderInfo` is enforced by the macOS kernel, not by oc-rsync.

## 9. Cross-references

- XAP-1 direction-matrix spec - `docs/audit/acl-xattr-direction-matrix.md`
  (PR #4901).
- XAP-3 xattr harness primitive - pending.
- XAP-5 macOS -> Linux xattr round-trip test - pending.
- WPC-1 ADS handling audit - `docs/audit/windows-ads-handling.md` (PR #4898).
- `apple-fs` roundtrip audit (prior, narrower scope) -
  `docs/audits/apple-fs-roundtrip.md`.
- Memory note: `[[project_xattr_acl_cross_platform_parity_gap]]`.
