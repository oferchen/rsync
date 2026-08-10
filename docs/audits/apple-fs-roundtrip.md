# apple-fs ResourceFork / HFS+ xattr round-trip audit

Task: #1841. Branch: `docs/apple-fs-roundtrip-audit`.

## Scope

Audit the `apple-fs` crate and its surrounding xattr pipeline for round-trip
fidelity of macOS-specific extended attributes - in particular
`com.apple.ResourceFork`, `com.apple.FinderInfo`, the `com.apple.metadata:*`
namespace, AppleDouble (`._foo`) sidecar files, and the `XATTR_NOFOLLOW` /
position-argument semantics of macOS `getxattr(2)` / `setxattr(2)`.

Source files inspected (all paths repository-relative):

- `crates/apple-fs/src/lib.rs`
- `crates/apple-fs/Cargo.toml`
- `crates/apple-fs/README.md`
- `crates/metadata/src/xattr.rs` (xattr listing/read/write/sync)
- `crates/metadata/src/xattr_stub.rs` (non-xattr feature stub)
- `crates/protocol/src/xattr/{mod,prefix,entry,list,cache}.rs`
- `crates/protocol/src/xattr/wire/{mod,encode,decode,types,tests}.rs`
- `crates/transfer/src/receiver/directory/mod.rs`
- `crates/engine/src/local_copy/executor/cleanup.rs`
- `crates/engine/src/local_copy/clonefile.rs`
- `crates/fast_io/src/platform_copy/dispatch.rs`

Upstream references consulted (per project rules - upstream C is the source of
truth):

- `xattrs.c` (`rsync_xal_get`, `get_xattr`, `set_xattr`, `send_xattr_name`,
  `receive_xattr`)
- The macOS `getxattr(2)` / `setxattr(2)` / `listxattr(2)` man pages, with
  attention to the `position` and `options` arguments. Mainline rsync 3.4.1
  has no `ResourceFork` pathway; macOS exposes the resource fork as an xattr
  named `com.apple.ResourceFork`, and rsync reads/writes it via the standard
  xattr APIs.

## TL;DR

The `apple-fs` crate is a thin Unix portability shim for `mkfifo`, `mknod`,
and HFS+/APFS NFD->NFC filename normalization. It deliberately exposes no
ResourceFork, FinderInfo, AppleDouble, or `XATTR_NOFOLLOW` surface. All
macOS-specific xattr handling is delegated to the third-party `xattr` crate
(v1.6.x) via `crates/metadata/src/xattr.rs`. Round-trip of
`com.apple.ResourceFork` / `com.apple.FinderInfo` therefore depends on the
`xattr` crate's macOS implementation and on `metadata`'s plumbing - not on
anything in `apple-fs`. The audit found no Critical or High issues. There is
one Medium finding (no AppleDouble (`._foo`) helper-file filter on a non-macOS
receiver) and several Info-level observations that map cleanly to upstream's
own behaviour.

## Surface area of `apple-fs`

| `apple-fs` symbol | Sender-side use | Receiver-side use | Status |
|-------------------|-----------------|-------------------|--------|
| `mkfifo(path, mode)` | n/a | `metadata::special::create_fifo_inner` recreates FIFOs from the file list. | OK. Unix-only. Returns `io::ErrorKind::Unsupported` on non-Unix. |
| `mknod(path, mode, device)` | n/a | `metadata::special::create_device_node_inner` recreates char/block devices and FIFOs. | OK. Unix-only. Returns `io::ErrorKind::Unsupported` on non-Unix. Mode width difference between Apple (`u16`) and other Unix (`u32`) is handled by `libc::mode_t`. |
| `normalize_filename(name)` | `engine::local_copy::executor::cleanup` normalizes file-list names before delete-pass comparison. | `transfer::receiver::directory` normalizes `read_dir` results before quick-check / delete-pass. | OK. NFC pass on macOS only; identity stub on every other platform. |

The crate has no other public items, no `#[cfg(target_os = "macos")]` branches
beyond `normalize_filename`, and no FFI bindings to ResourceFork-, FinderInfo-,
or `_PATH_RSRCFORKSPEC` (`/..namedfork/rsrc`)-related APIs. By design it is not
the home for ResourceFork plumbing.

## Round-trip path (where ResourceFork actually flows)

1. **Sender, list xattrs** - `metadata::xattr::list_attributes(path,
   follow_symlinks)` calls the `xattr` crate's `xattr::list` / `xattr::list_deref`.
   On macOS the underlying syscall is `listxattr(2)` with `XATTR_NOFOLLOW` set
   when `follow_symlinks` is false. `com.apple.ResourceFork`,
   `com.apple.FinderInfo`, `com.apple.quarantine`, `com.apple.metadata:*`, and
   any user-defined `com.apple.*` attributes appear here as ordinary strings.
2. **Sender, read each xattr** - `read_attribute` calls `xattr::get` /
   `xattr::get_deref`, which in turn invoke `getxattr(2)` with `position = 0`
   and a buffer sized via the standard "ask for size, allocate, read" pattern
   exposed by the `xattr` crate. On macOS, `getxattr` with `position = 0`
   returns the entire xattr value in one call regardless of size, including
   the entire resource fork. This is the same pattern that mainline rsync
   3.4.1 uses (`xattrs.c:rsync_xal_get` -> `sys_lgetxattr`); upstream does not
   loop over `position` either. FinderInfo is always 32 bytes.
3. **Sender, wire encode** - `read_xattrs_for_wire` translates names via
   `local_to_wire`. On non-Linux (macOS) the translator is a pass-through, so
   `com.apple.ResourceFork` and `com.apple.FinderInfo` cross the wire under
   their original names. Values larger than `MAX_FULL_DATUM = 32` bytes are
   abbreviated to a 16-byte MD5 checksum on the wire and the full payload is
   only sent when the receiver requests it. This matches upstream's
   abbreviation protocol exactly.
4. **Receiver, wire decode** - `wire_to_local` performs the inverse mapping.
   On non-Linux it is again a pass-through. On Linux the bytes
   `com.apple.ResourceFork` arrive without a `user.` prefix and are remapped
   to `user.com.apple.ResourceFork` via the unconditional `user.*` prefix
   addition for non-`rsync.`-prefixed wire names. Linux can store user xattrs
   of arbitrary size (subject to filesystem limits; ext4's small inline cap
   does not apply once the value migrates to a separate block).
5. **Receiver, apply xattrs** - `apply_xattrs_from_list` writes via
   `xattr::set` / `xattr::set_deref`, which on macOS calls `setxattr(2)` with
   `position = 0` and `options = XATTR_NOFOLLOW` (or `0` when following).
   `com.apple.FinderInfo` of length 32 and `com.apple.ResourceFork` of any
   size are applied in a single call - macOS accepts either as long as the
   `position` and `options` are consistent.

The end-to-end path is symmetric. The `apple-fs` crate is not in this hot
path beyond `normalize_filename` (filename comparison, not xattr value
handling).

## Findings

### Info-1 - apple-fs name is broader than the implementation

Severity: Info.

The crate is named `apple-fs` and the `README.md` describes it as "filesystem
primitives required by `oc-rsync` when interacting with Apple platforms",
yet the public surface is `mkfifo`, `mknod`, and `normalize_filename`. None
of these are Apple-specific - `mkfifo` and `mknod` are POSIX, and the only
macOS-only function is `normalize_filename`. The workspace `README.md:223`
also describes the crate as "macOS filesystem operations (clonefile,
FSEvents)", but the actual `clonefile` wrappers live in
`crates/engine/src/local_copy/clonefile.rs` and
`crates/fast_io/src/platform_copy/dispatch.rs`. There is no FSEvents code
anywhere in the workspace. This is documentation drift, not a correctness
issue.

Recommendation: leave the crate name (renaming would churn many
`Cargo.toml`s) but tighten the `README.md` and the workspace `README.md`
description to match what the crate actually exports. Tracked as a follow-up
(see Follow-ups F-1).

### Info-2 - No special-case xattr name interception

Severity: Info.

`crates/metadata/src/xattr.rs::is_xattr_permitted` does not single out any
`com.apple.*` name. On Linux it filters by namespace prefix
(`user.`/`system.`); on every other Unix it returns `true` unconditionally.
This matches upstream rsync's `xattrs.c` exactly - upstream relies on the
kernel's xattr ACLs and on the `--filter` machinery, not on hard-coded
allow-lists. No change needed.

### Info-3 - Position argument is implicit

Severity: Info.

macOS `getxattr(2)` / `setxattr(2)` take a `position` argument that is only
meaningful for the resource fork. The `xattr` crate v1.6 hard-codes
`position = 0` and `options = XATTR_NOFOLLOW` (when not following symlinks).
This matches upstream rsync, which also passes `0` (`xattrs.c` reads the
attribute in one shot via `sys_lgetxattr` -> `lgetxattr`). For pathological
resource forks larger than the kernel's per-call buffer cap, both upstream
and oc-rsync would in principle truncate. In practice macOS does not enforce
a per-call cap on `getxattr` for resource forks; the entire fork is
returned. No change needed.

### Medium-1 - AppleDouble sidecar files are not filtered

Severity: Medium.

When macOS writes a `com.apple.ResourceFork` or `com.apple.FinderInfo`
xattr to a filesystem that does not support extended attributes (SMB, FAT,
some NFS exports, older Linux NFS clients), the OS materializes them as
AppleDouble `._foo` files alongside the original `foo`. Upstream rsync does
not filter these by default either, but documents `--exclude='.fseventsd'`
and `--exclude='._*'` as common user-side filters. oc-rsync's filter engine
supports both patterns, but the workspace ships no preset / shorthand for
them and the audit could not find a documentation cross-reference.

This is not a round-trip bug per se - the bytes are preserved verbatim in
both directions - but it is an interoperability footgun:

- macOS sender, Linux receiver, default settings: AppleDouble files are
  copied as opaque payloads, doubling on-disk size for any resource-forked
  source.
- Linux sender (with previously imported AppleDouble files), macOS receiver:
  the `._foo` files are restored as plain files alongside `foo` rather than
  being merged back into `foo`'s xattrs. macOS does not auto-merge; the
  resource fork remains stranded.

Upstream's own answer is the (out-of-tree) `--fileflags` / `--crtimes` /
`--protect-args` patches, none of which are in mainline 3.4.1. Per project
policy ("Do not invent capabilities not in upstream rsync"), oc-rsync should
not add a new flag here. The right outcome is a documentation note and a
follow-up to consider an opt-in `--apple-double-merge` analogue if and only
if upstream picks it up.

Recommendation: add a `docs/platform-notes.md` entry; track follow-up F-2.

### Low-1 - `is_xattr_permitted` non-Linux branch ignores `system.*`

Severity: Low.

`crates/metadata/src/xattr.rs:38` returns `true` for every name on non-Linux
Unix. This is correct for macOS, where there is no `system.` namespace.
However, FreeBSD does have `system.posix1e.*` for ACLs. If an oc-rsync
binary built for FreeBSD is ever placed in the Apple round-trip path, the
non-Linux branch would happily ship FreeBSD `system.*` attributes through
the wire as ordinary `com.apple.*`-style names. Upstream rsync's
`xattrs.c:64-68` only filters `system.*` on Linux; FreeBSD users have to
filter manually too. So oc-rsync's behaviour matches upstream.

Recommendation: leave the code, document the non-Linux gap in this audit.

### Low-2 - `normalize_filename` only acts on macOS

Severity: Low.

`apple_fs::normalize_filename` is `#[cfg(target_os = "macos")]` for the NFC
implementation and an identity stub elsewhere. If a Linux receiver writes
to an HFS+ network share that re-normalizes filenames to NFD, the receiver
will not pre-normalize and may end up doing redundant transfers. This is a
known cross-platform rsync issue; upstream's answer is `--iconv`. No code
change here.

## Round-trip matrix

Test data (conceptual): a single regular file `app.bundle/Info.plist` with
`com.apple.FinderInfo = 32 bytes` and a 4 KiB `com.apple.ResourceFork`. All
runs use `-aX` (archive plus xattrs).

| Direction         | Names preserved | Values preserved | Notes |
|-------------------|-----------------|------------------|-------|
| macOS -> macOS   | Yes             | Yes              | `com.apple.*` cross the wire as-is via `local_to_wire`/`wire_to_local` non-Linux pass-through. APFS clonefile fast-path (`fast_io::platform_copy`) preserves xattrs verbatim when source and destination are on the same APFS volume; otherwise the regular xattr pipeline runs. |
| macOS -> Linux   | Remapped        | Yes              | On the wire the names arrive un-prefixed. `wire_to_local` adds `user.` so the destination stores them as `user.com.apple.FinderInfo` and `user.com.apple.ResourceFork`. Linux ext4/xfs accept arbitrary `user.*` payloads up to the per-attribute byte cap (xfs ~64 KiB, ext4 ~4 KiB inline / unlimited with `large_xattr` feature). Larger ResourceForks may fail with `ENOSPC` on a default ext4; the `metadata::xattr::write_attribute` error is propagated unchanged. |
| Linux -> macOS   | Restored        | Yes              | If the Linux sender was previously a macOS->Linux receiver, the xattrs are stored as `user.com.apple.*`. `local_to_wire` strips `user.` and the macOS receiver writes them under `com.apple.*` again. If the Linux sender is the original origin (xattrs created locally as `user.com.apple.*`), the same path applies. |
| macOS -> Windows | No              | No               | The Windows port has no xattr backend; `metadata::xattr_stub` returns `Ok(())` for sync calls. `com.apple.*` xattrs are silently dropped, which mirrors upstream rsync's behaviour on Windows (`cygwin`/`MSYS2` builds ship without xattr support). AppleDouble `._foo` files (if any) are preserved as plain files. |

Additional notes:

- Symlinks. `read_xattrs_for_wire(path, follow_symlinks=false, ...)` calls
  `xattr::list` (not `list_deref`), which on macOS sets `XATTR_NOFOLLOW`. This
  matches upstream rsync's lstat-aware xattr handling. Resource forks on
  symlinks are extremely rare and untested in either codebase.
- Hard links. `engine::local_copy` applies xattrs to one inode; subsequent
  hard-link siblings share the same xattrs by virtue of sharing the inode.
- Permissions / ACLs. `com.apple.metadata:*` and `com.apple.security:*` are
  treated identically to user-defined xattrs; nothing in `metadata::acl`
  intercepts them. ACLs use `system.posix_acl_*` (Linux) or the macOS
  `acl_get_link_np` / `acl_set_link_np` wrappers in `metadata::acl`.

## Wiring check

`apple-fs` symbols are reachable from both the sender and receiver halves of
every transfer mode:

- `mkfifo` / `mknod`: only used during file-list materialization on the
  receiver (via `metadata::special::create_special_file`). Sender side does
  not invoke them.
- `normalize_filename`: used on both sides during file-list comparison
  (sender: cleanup pass; receiver: directory diff). Symmetric.

ResourceFork / FinderInfo flow does NOT pass through `apple-fs`. It passes
through `metadata::xattr` (read), `protocol::xattr::wire` (encode/decode),
and `metadata::xattr::apply_xattrs_from_list` (write). All three sides
behave consistently on macOS, Linux, and the xattr-stubbed Windows path.
There is no missing wiring.

## Follow-ups

- F-1 (Info, doc-only): Update `crates/apple-fs/README.md` and the
  workspace `README.md:223` line to remove the misleading "clonefile, FSEvents"
  description. The actual scope is `mkfifo` / `mknod` / NFC normalization.
- F-2 (Medium, design): Decide whether to add an opt-in
  `--apple-double-merge` (or equivalent) once or if upstream rsync picks up
  the relevant patch. Until then document the AppleDouble interop footgun in
  `docs/platform-notes.md`.
- F-3 (Info, test): Add a macOS-only nextest that creates a temp file,
  writes `com.apple.FinderInfo` and a small `com.apple.ResourceFork`, and
  exercises `metadata::xattr::read_xattrs_for_wire` ->
  `protocol::xattr::wire::send_xattr` ->
  `protocol::xattr::wire::recv_xattr` -> `metadata::xattr::apply_xattrs_from_list`
  end to end. The current test suite covers each leg in isolation but not
  the full round-trip with macOS-specific names.
- F-4 (Low, doc): Note in `docs/platform-notes.md` that on Linux receivers
  the `com.apple.ResourceFork` payload is stored under
  `user.com.apple.ResourceFork`, and that some kernels enforce a per-attribute
  size cap that smaller-than-mainline-rsync may surface as `ENOSPC`.

## Conclusion

The `apple-fs` crate, as it exists today, is correctly scoped: it does not
own the ResourceFork pipeline and does not need to. The actual xattr
round-trip path (`metadata` -> `protocol::xattr::wire` -> `metadata`) handles
`com.apple.ResourceFork`, `com.apple.FinderInfo`, and the entire
`com.apple.*` namespace symmetrically via the third-party `xattr` crate's
macOS implementation, which already passes `XATTR_NOFOLLOW` and `position = 0`
in line with upstream rsync 3.4.1. No in-PR code fix is warranted; the four
follow-ups above are documentation and test-coverage items.
