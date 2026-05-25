# Windows NTFS reparse-point classification audit (WPC-7)

Tracks parent #2869 (Windows real-world parity series). Feeds
follow-ups #2910 (WPC-8: ship the classifier) and #2911 (WPC-9:
regression tests). Companion to the high-level matrix shipped in
#4920 (WPC-13, `docs/user/windows-support-matrix.md`).

## 1. Scope

This audit characterises how oc-rsync recognises and processes NTFS
reparse points today. The aim is to enumerate every tag oc-rsync
distinguishes from a regular file or directory, every tag that
collapses silently to a default path, and the user-visible
consequence in each case. The artefact informs the WPC-8 classifier
implementation (`crates/metadata/src/windows/reparse.rs`) and the
WPC-9 regression-test surface.

Out of scope: the long-path (`\\?\`) prefix work (WPC-5/6), DACL
inheritance (WPC-10), case-insensitive collision detection
(WPC-11), and the attribute-bit mapping for hidden/system/archive
(WPC-12). Reparse classification is orthogonal to all of those.

## 2. Background on reparse points

An NTFS reparse point is a small block of metadata
(`REPARSE_DATA_BUFFER` in `winnt.h`) attached to a file or
directory. The first 32-bit word is the **reparse tag**, a value
allocated by Microsoft that identifies the owner of the buffer and
the meaning of the payload that follows. The `FILE_ATTRIBUTE_REPARSE_POINT`
bit in `GetFileAttributesW` is set whenever a path carries a reparse
point, regardless of tag.

A file or directory may carry at most one reparse point at a time;
the kernel chains the reparse handler associated with the tag into
the I/O path so an `open(path)` call can be redirected (symlinks,
junctions, OneDrive hydration) or intercepted (HSM, WSL POSIX
translation, AppExecLink). The raw payload is read with
`DeviceIoControl(handle, FSCTL_GET_REPARSE_POINT, ...)`.

Common tags from `winnt.h` and the Microsoft reparse-tag registry:

| Constant | Hex | Owner |
|---|---|---|
| `IO_REPARSE_TAG_MOUNT_POINT` | `0xA000_0003` | NTFS junctions and volume mount points |
| `IO_REPARSE_TAG_HSM` | `0xC000_0004` | Legacy hierarchical-storage manager |
| `IO_REPARSE_TAG_HSM2` | `0x8000_0006` | HSM v2 |
| `IO_REPARSE_TAG_SIS` | `0x8000_0007` | Single-Instance Storage |
| `IO_REPARSE_TAG_WIM` | `0x8000_0008` | Windows Imaging Format files |
| `IO_REPARSE_TAG_DFS` | `0x8000_000A` | Distributed File System |
| `IO_REPARSE_TAG_SYMLINK` | `0xA000_000C` | Genuine NTFS symbolic link |
| `IO_REPARSE_TAG_DFSR` | `0x8000_0012` | DFS replication |
| `IO_REPARSE_TAG_DEDUP` | `0x8000_0013` | Server data deduplication |
| `IO_REPARSE_TAG_NFS` | `0x8000_0014` | NFS-server-exported file |
| `IO_REPARSE_TAG_FILE_PLACEHOLDER` | `0x8000_0015` | Original OneDrive placeholder (pre-Win10 1709) |
| `IO_REPARSE_TAG_WOF` | `0x8000_0017` | Windows Overlay Filter (compact OS) |
| `IO_REPARSE_TAG_WCI` | `0x8000_0018` | Windows Container Isolation |
| `IO_REPARSE_TAG_GLOBAL_REPARSE` | `0xA000_0019` | Bind-mount equivalent |
| `IO_REPARSE_TAG_CLOUD` family | `0x9000_001A` ... `0x9000_031A` | Cloud Files API placeholders (OneDrive on-demand, Dropbox, third parties) |
| `IO_REPARSE_TAG_APPEXECLINK` | `0x8000_001B` | Windows Store app-execution alias |
| `IO_REPARSE_TAG_PROJFS` | `0x9000_001C` | Projected File System (VFS-for-Git, GVFS) |
| `IO_REPARSE_TAG_LX_SYMLINK` | `0xA000_001D` | WSL POSIX symlink shadow |
| `IO_REPARSE_TAG_AF_UNIX` | `0x8000_0023` | WSL AF_UNIX socket shadow |
| `IO_REPARSE_TAG_LX_FIFO` | `0x8000_0024` | WSL FIFO shadow |
| `IO_REPARSE_TAG_LX_CHR` | `0x8000_0025` | WSL character-device shadow |
| `IO_REPARSE_TAG_LX_BLK` | `0x8000_0026` | WSL block-device shadow |

The tag namespace is partitioned by the high bit: tags with bit 31
set (`0x8000_xxxx`, `0x9000_xxxx`, `0xA000_xxxx`, `0xC000_xxxx`)
are Microsoft-owned reparse handlers; the kernel treats them
authoritatively. Bit 29 distinguishes "surrogate" tags - reparse
points that present as the underlying object type after kernel
redirection (symlinks, mount points). Without that bit the reparse
is treated as the file itself, not as a redirection.

## 3. Inventory of current reparse-point handling

`ripgrep` across the workspace for any of `REPARSE`, `ReparseTag`,
`IO_REPARSE_TAG`, `FILE_ATTRIBUTE_REPARSE_POINT`,
`FSCTL_GET_REPARSE_POINT`, or `DeviceIoControl` (excluding the
non-reparse use in `crates/fast_io/src/platform_copy/dispatch.rs`
where `DeviceIoControl` is used for ReFS reflink) returns the
following hits:

- `crates/metadata/src/xattr_windows.rs:190` -
  `/// Unix backend; NTFS reparse points are always traversed by the`
- `crates/metadata/src/xattr_windows.rs:256` -
  `/// `_follow_symlinks` is ignored on Windows; reparse-point traversal is`
- `crates/metadata/src/xattr_windows.rs:299` - same comment on `write_attribute`.
- `crates/metadata/src/xattr_windows.rs:343` - same comment on `remove_attribute`.

That is the complete inventory. The Windows xattr backend documents
that it always traverses reparse points - the `_follow_symlinks`
parameter is accepted only for API parity and is discarded. There
is no tag dispatch anywhere in the workspace; no code reads a
reparse buffer with `FSCTL_GET_REPARSE_POINT`; no code branches on
`IO_REPARSE_TAG_*`. The `windows` crate import surface in
`crates/metadata/src/copy_as.rs` and `crates/metadata/src/acl_windows/`
imports only DACL, SID, and privilege types; no reparse-related
constants are pulled in. The `windows-sys` imports under
`crates/fast_io/src/iocp/` and `crates/fast_io/src/platform_copy/`
similarly pull only `CreateFileW`, `GetFinalPathNameByHandleW`,
overlapped I/O, and the ReFS reflink ioctl.

Every classification of a reparse-tagged path therefore goes
through Rust's `std::fs::Metadata::file_type()` and the three boolean
predicates `is_file()` / `is_dir()` / `is_symlink()`. Those
predicates are evaluated at the file-list-build, walk, and apply
sites listed below.

### 3.1 File-list build sites

- `crates/transfer/src/generator/file_list/entry.rs:82` -
  `else if file_type.is_symlink()` branch wraps the entry as
  `FileEntry::new_symlink` with `std::fs::read_link(full_path)` as the target.
- `crates/transfer/src/generator/file_list/walk.rs:121,291,374` -
  `std::fs::read_link(...)` on the symlink branch; the walk path
  treats every `is_symlink()` entry uniformly.
- `crates/transfer/src/generator/file_list/walk.rs:361-392` -
  `resolve_symlink_metadata()` chooses between `fs::metadata` and
  `fs::symlink_metadata` based on the `--copy-links` /
  `--copy-unsafe-links` flags. No tag inspection.
- `crates/flist/src/file_list_walker.rs:106-149` - same dispatch.
  Lines 117 and 138 call `fs::read_link` / `fs::metadata` on the
  symlink branch.
- `crates/flist/src/parallel.rs:137,324,434` and
  `crates/flist/src/batched_stat/cache.rs:105` - all use
  `fs::symlink_metadata` for lstat-style inspection.

### 3.2 Apply / create-symlink sites

- `crates/engine/src/local_copy/executor/special/symlink.rs:464-481`
  - `create_symlink()`. Unix calls `std::os::unix::fs::symlink`.
    Windows dispatches between `symlink_dir` and `symlink_file`
    based on `source.metadata()` (line 477); falls back to
    `symlink_file` when the source metadata is unreadable.
- `crates/transfer/src/receiver/directory/links.rs:33-160` -
  `create_symlinks()` for the receiver path. **`#[cfg(unix)]`-gated**.
  Lines 162-169 are the Windows no-op stub: the receiver does not
  reconstruct symlinks on Windows at all.
- `crates/batch/src/replay/fs_ops.rs:51-96` - batch replay's
  `create_symlink()`. Windows branch (lines 76-88) hard-codes
  `symlink_file`; the comment on line 75 acknowledges the
  directory-symlink detection is missing.

### 3.3 What the inventory means

The receiver-side create path on Windows is a no-op, so symlinks
that survive the wire arrive but are not materialised. The
local-copy executor and the batch replay do create Windows
symlinks, both with `std::os::windows::fs::symlink_{file,dir}`,
which under the hood call `CreateSymbolicLinkW` with the
`SYMBOLIC_LINK_FLAG_DIRECTORY` bit set only when the variant
chosen is `symlink_dir`. The flag selection in the executor relies
on probing the **source** metadata; the batch replay relies only on
the wire-decoded target path (and so always picks `symlink_file`).
Neither path consults the reparse tag of the source object.

## 4. Symlink path (`IO_REPARSE_TAG_SYMLINK`)

A genuine NTFS symbolic link carries `IO_REPARSE_TAG_SYMLINK`
(`0xA000_000C`). Rust's `std::fs::FileType::is_symlink()` on Windows
returns `true` only for this tag and (since Rust 1.49) also for
`IO_REPARSE_TAG_MOUNT_POINT`; every other reparse tag is reported as
a regular file or directory according to the
`FILE_ATTRIBUTE_DIRECTORY` bit.

### 4.1 Detection on the sender / file-list side

Detection is implicit: `fs::symlink_metadata()` reads the lstat-like
metadata and `file_type.is_symlink()` evaluates to true when the
underlying kernel surfaces a symlink reparse tag. The sender pipeline
in `crates/transfer/src/generator/file_list/entry.rs:82` and
`crates/flist/src/file_list_walker.rs:138` then routes the entry to
the symlink branch. **`FSCTL_GET_REPARSE_POINT` is never invoked;**
the link target comes from `std::fs::read_link()`, which on Windows
calls `CreateFileW(... FILE_FLAG_OPEN_REPARSE_POINT ...)` followed by
`DeviceIoControl(FSCTL_GET_REPARSE_POINT)` internally and returns the
substitute-name UTF-16 string from the reparse payload, normalised
to forward slashes by `Path::from_raw`.

### 4.2 Target type preservation (file vs directory)

Windows symbolic links are typed at creation time. The receiver-side
`create_symlinks` in `crates/transfer/src/receiver/directory/links.rs`
is `#[cfg(unix)]` only and does nothing on Windows; the link is
silently dropped. The local-copy executor path
(`crates/engine/src/local_copy/executor/special/symlink.rs:476-480`)
probes the **source** path's metadata with `source.metadata()`
(which follows the link to the target on Windows) to choose
between `symlink_dir` and `symlink_file`. The probe inherits all
the failure modes of stat-after-walk: the target may have moved,
been deleted, been replaced, or live on a different volume. When
the probe fails (line 479) the fallback is `symlink_file`,
collapsing the type to file even when the source link was a
directory link.

### 4.3 Conclusion for the symlink path

Direction matrix:

- **Linux source -> Windows destination via local copy**: source
  is identified through std's `is_symlink()`; target type probe
  uses the **resolved source path**, which on a cross-mounted Linux
  source over Windows is meaningless because the resolution
  happens against the Linux source layout. Type selection is
  best-effort.
- **Windows source -> Linux destination**: symlink is recognised
  and `fs::read_link` returns the UTF-16 target; on Linux,
  symlinks are typeless, so the receiver creates a POSIX symlink
  with the decoded target path. UNC and drive-letter prefixes flow
  through verbatim, which renders most absolute-path Windows
  symlinks broken on the destination.
- **Windows source -> Windows destination via receiver loop**:
  silently no-op (line 164). This is a hole: a `--server` receive
  on Windows drops symlinks even when `-l` is set.
- **Windows source -> Windows destination via batch replay**:
  always creates a file symlink, even if the source was a
  directory link (`fs_ops.rs:78`).

## 5. Junction / mount-point path (`IO_REPARSE_TAG_MOUNT_POINT`)

`IO_REPARSE_TAG_MOUNT_POINT` (`0xA000_0003`) covers two distinct
use-cases that share a tag: NTFS junctions (a directory pointing
to another directory on the same volume) and volume mount points
(a directory pointing to a different volume's root). The kernel
treats both as directory redirections.

Since Rust 1.49 (https://github.com/rust-lang/rust/pull/74373)
`std::fs::FileType::is_symlink()` returns `true` for
`IO_REPARSE_TAG_MOUNT_POINT` as well. That means:

- `fs::symlink_metadata()` on a junction reports `is_symlink() == true`.
- `fs::read_link()` on a junction returns the substitute-name path
  from the reparse buffer; that path is rendered in Windows native
  form (`\\?\Volume{GUID}\` for mount points,
  `\??\C:\target\path` for junctions) and is **not** the form a
  POSIX `readlink()` would produce.
- The file-list build therefore tags the entry as a symlink with a
  Windows-native target path. The wire encoding stores that path
  verbatim.

### 5.1 Direction-matrix consequences

- **Windows source with `-l`**: junction is shipped as a symlink
  with a `\??\` or `\\?\Volume{...}\` prefixed target. The
  receiver, when on POSIX, creates a `symlink()` to that string,
  which is meaningless on Linux/macOS.
- **Windows destination via local-copy executor**: `create_symlink`
  calls `source.metadata()` (line 477); since the junction is a
  directory reparse, the metadata follows the redirection and the
  branch selects `symlink_dir`. The result is a true NTFS symlink
  (tag `IO_REPARSE_TAG_SYMLINK`), not a junction. The destination
  is functionally equivalent for ordinary file access but loses
  the junction-vs-symlink distinction.
- **Loop detection**: a junction can point to an ancestor of its
  own path (`C:\Users\Public\Junction -> C:\Users`). Because the
  file-list walker treats junctions as symlinks, it will not
  recurse into them by default (the same way it skips POSIX
  symlinks unless `--copy-links`). However, when `--copy-links`
  is active, `fs::metadata` follows the redirection and the walk
  will descend, creating a re-entry / infinite-recursion risk that
  upstream rsync on Cygwin avoids only because Cygwin maps
  junctions back to themselves.

Upstream rsync's Cygwin port relies on the Cygwin POSIX layer
which surfaces both junctions and NTFS symlinks via `lstat()` with
`S_IFLNK`. That conflation is functionally what oc-rsync does today,
but oc-rsync does it through Rust's std rather than through Cygwin
emulation.

## 6. Cloud-placeholder path (`IO_REPARSE_TAG_CLOUD*`)

The Cloud Files API (`cldapi.dll`, Windows 10 1709+) reserves the
tag range `0x9000_001A` ... `0x9000_031A` for placeholder files
created by OneDrive Files-On-Demand, Dropbox Smart Sync, and
similar providers. The bit-29 surrogate flag is **not** set, so the
kernel does not auto-redirect; instead it signals the cloud
provider when a non-placeholder operation is attempted, and the
provider hydrates the file in place before the operation returns.

### 6.1 Current oc-rsync behaviour

- `std::fs::symlink_metadata()` reports the placeholder as a
  regular file with `FILE_ATTRIBUTE_REPARSE_POINT` set. The Rust
  predicate `is_symlink()` returns **false** (the tag is not in
  the small list std recognises).
- `is_file()` returns true; the file-list build path takes the
  `FileEntry::new_file(..., metadata.len(), mode)` branch with
  the placeholder's logical size (the size the user sees, not the
  on-disk footprint of the placeholder stub).
- The sender then opens the file for read. Opening the file
  triggers cloud-provider hydration: OneDrive will block the
  `CreateFileW` call until the file body has been downloaded from
  the cloud and materialised on local disk. The user perceives
  this as a slow stat followed by a normal transfer.

For a **backup** workflow this is correct: the backup needs the
real file bytes, not the placeholder stub. For a **mirror /
sync** workflow this is destructive: oc-rsync will rehydrate every
dehydrated file in the tree on every run, defeating the entire
purpose of Files-On-Demand. There is no opt-out today.

### 6.2 Network and storage cost

Hydration runs at the network's expense and at the local disk's
expense. A 10 TB OneDrive root sync forces a 10 TB download even
when oc-rsync would have skipped most files by mtime+size. There
is also no path to preserve the placeholder reparse buffer on the
destination, even if the destination is another NTFS volume with
the same cloud provider configured.

## 7. WSL Linux-symlink path (`IO_REPARSE_TAG_LX_SYMLINK`)

WSL stores POSIX symlinks on NTFS using the `IO_REPARSE_TAG_LX_SYMLINK`
tag (`0xA000_001D`). The reparse buffer payload is the raw POSIX
target string in UTF-8 (no `\??\` prefix, no UTF-16 conversion).
Bit 29 is **not** set, so Rust's `is_symlink()` returns false.

### 7.1 Current oc-rsync behaviour

- `fs::symlink_metadata()` reports the entry as a regular file
  (no `FILE_ATTRIBUTE_DIRECTORY` bit on WSL symlinks).
- The file-list build takes the `is_file()` branch and records
  the entry as a regular file with whatever size the kernel
  surfaces - which for a WSL symlink is typically the byte length
  of the target string padded by the reparse-buffer header, often
  reported as zero on FAT-style enumerations.
- The sender opens the file for read. WSL's reparse handler
  returns the raw target bytes as the file content (this is what
  `cat /proc/self/maps` style WSL behaviour relies on), so the
  wire payload is the target path encoded as a "regular" file
  body.
- The receiver materialises a regular file containing the target
  string. The symlink is lost.

This is silently incorrect: oc-rsync converts WSL POSIX symlinks
into regular text files. Upstream rsync running inside WSL itself
sees the symlinks correctly because the WSL kernel surfaces them
as POSIX symlinks; upstream rsync running on Cygwin against a
WSL-populated NTFS tree exhibits the same loss-of-symlink that
oc-rsync exhibits.

### 7.2 Sibling WSL tags

`IO_REPARSE_TAG_AF_UNIX` (`0x8000_0023`),
`IO_REPARSE_TAG_LX_FIFO` (`0x8000_0024`),
`IO_REPARSE_TAG_LX_CHR` (`0x8000_0025`), and
`IO_REPARSE_TAG_LX_BLK` (`0x8000_0026`) similarly carry POSIX
sockets / FIFOs / device nodes. None are recognised today; all
collapse to regular files. The lost information is the device
major/minor for char/block, the FIFO marker, or the socket marker.

## 8. Findings

### F1. Tags actively recognised by oc-rsync today

| Tag | Recognised as | Evidence |
|---|---|---|
| `IO_REPARSE_TAG_SYMLINK` (`0xA000_000C`) | Symlink (via std) | `entry.rs:82`, `walk.rs:121,291,374` |
| `IO_REPARSE_TAG_MOUNT_POINT` (`0xA000_0003`) | Symlink (via std, since Rust 1.49) | same call sites |

That is the complete set. Every other tag in the Microsoft
registry collapses to either "file" (no `FILE_ATTRIBUTE_DIRECTORY`)
or "directory" (with `FILE_ATTRIBUTE_DIRECTORY`) without further
distinction.

### F2. Tags that fall through to the default path

| Tag | Falls through as | Consequence |
|---|---|---|
| `IO_REPARSE_TAG_CLOUD*` | Regular file | Triggers cloud hydration on every transfer; placeholders are rematerialised whether the user wanted that or not. |
| `IO_REPARSE_TAG_LX_SYMLINK` | Regular file | WSL POSIX symlink is shipped as a regular file whose content is the target path string; symlink is lost on the destination. |
| `IO_REPARSE_TAG_AF_UNIX` / `LX_FIFO` / `LX_CHR` / `LX_BLK` | Regular file | WSL POSIX sockets, FIFOs, and device nodes are shipped as regular files; type information is lost. |
| `IO_REPARSE_TAG_APPEXECLINK` (`0x8000_001B`) | Regular file | Windows Store app-execution alias is read as a regular file (the reparse buffer happens to be readable through `CreateFileW`); the destination receives an opaque blob that is non-functional outside the Store app context. |
| `IO_REPARSE_TAG_WCI` (`0x8000_0018`) | File or directory per `FILE_ATTRIBUTE_DIRECTORY` | Container-isolated path is shipped as if it were the underlying object; container layering metadata is lost. |
| `IO_REPARSE_TAG_GLOBAL_REPARSE` (`0xA000_0019`) | File or directory | Bind-mount equivalent collapses to the underlying object. |
| `IO_REPARSE_TAG_HSM` / `HSM2` / `SIS` / `WIM` / `DEDUP` / `WOF` / `PROJFS` / `NFS` / `DFS` / `DFSR` | File or directory | Provider-specific reparse handlers; oc-rsync forces hydration / pulls dedup'd content; the on-disk space savings are lost on the destination. |

### F3. Silent classification errors with operational risk

- **Junction loops**: junctions that point to an ancestor of their
  own path are not detected as loops. The walker only avoids
  re-entry because junctions are treated as symlinks and symlinks
  are not followed by default. The first time a user passes
  `--copy-links` over a tree containing a self-referential
  junction, the walk recurses without bound.
- **WSL symlinks as files**: the wire format records a file with
  size N containing the POSIX target string. A reverse transfer
  back into a WSL tree will not restore the symlink; the source
  tree is silently degraded.
- **Cloud placeholders forcing hydration**: any oc-rsync run over
  a OneDrive root with Files-On-Demand enabled will redownload
  every dehydrated file. There is no diagnostic and no opt-out.
- **Receiver no-op on Windows**: `links.rs:162-169` drops all
  symlinks silently on a Windows-server-side receive. The matrix
  in `docs/user/windows-support-matrix.md` lists `-l` as Partial,
  but the partial-ness is documented only for the local-copy and
  batch paths.

### F4. Missing `IO_REPARSE_TAG_*` constants

The workspace defines none of the `IO_REPARSE_TAG_*` constants. A
canonical list from `winnt.h` (Windows SDK 10.0.22621) that the
WPC-8 classifier must enumerate:

- Surrogate tags (bit 29 set, the kernel transparently redirects):
  `IO_REPARSE_TAG_MOUNT_POINT`, `IO_REPARSE_TAG_SYMLINK`,
  `IO_REPARSE_TAG_GLOBAL_REPARSE`, `IO_REPARSE_TAG_LX_SYMLINK`.
- Microsoft non-surrogate tags (the kernel hands the I/O to a
  filter driver): `IO_REPARSE_TAG_HSM`, `IO_REPARSE_TAG_HSM2`,
  `IO_REPARSE_TAG_DRIVE_EXTENDER`, `IO_REPARSE_TAG_HOLOGRAPHIC`,
  `IO_REPARSE_TAG_SIS`, `IO_REPARSE_TAG_WIM`, `IO_REPARSE_TAG_CSV`,
  `IO_REPARSE_TAG_DFS`, `IO_REPARSE_TAG_FILTER_MANAGER`,
  `IO_REPARSE_TAG_DFSR`, `IO_REPARSE_TAG_DEDUP`, `IO_REPARSE_TAG_NFS`,
  `IO_REPARSE_TAG_FILE_PLACEHOLDER`, `IO_REPARSE_TAG_WOF`,
  `IO_REPARSE_TAG_WCI`, `IO_REPARSE_TAG_WCI_1`,
  `IO_REPARSE_TAG_CLOUD` through `IO_REPARSE_TAG_CLOUD_F`
  (sixteen-entry range), `IO_REPARSE_TAG_APPEXECLINK`,
  `IO_REPARSE_TAG_PROJFS`, `IO_REPARSE_TAG_LX_FIFO`,
  `IO_REPARSE_TAG_LX_CHR`, `IO_REPARSE_TAG_LX_BLK`,
  `IO_REPARSE_TAG_AF_UNIX`, `IO_REPARSE_TAG_STORAGE_SYNC`,
  `IO_REPARSE_TAG_WCI_TOMBSTONE`, `IO_REPARSE_TAG_UNHANDLED`,
  `IO_REPARSE_TAG_ONEDRIVE`, `IO_REPARSE_TAG_PROJFS_TOMBSTONE`.

The classifier need not enumerate every entry; it must collapse
the families it does not implement into `ReparseKind::Other(tag)`
so callers can log the raw tag and fall back to a documented
default.

## 9. WPC-8 acceptance criteria

The follow-up implementation under #2910 must land:

- **Module location**: `crates/metadata/src/windows/reparse.rs`,
  gated `#[cfg(windows)]`. Re-exported from `crates/metadata/src/lib.rs`
  alongside the existing `acl_windows` module.
- **Public enum**:

  ```rust
  pub enum ReparseKind {
      Symlink,
      MountPoint,
      Junction,
      Cloud(u32),
      AppExecLink,
      Wci,
      GlobalReparse,
      LxSymlink,
      LxFifo,
      LxChr,
      LxBlk,
      AfUnix,
      ProjFs,
      Wof,
      Hsm,
      Other(u32),
  }
  ```

  `MountPoint` and `Junction` are distinguished by inspecting the
  substitute-name prefix in the reparse buffer
  (`\??\Volume{...}\` indicates a volume mount point, `\??\<drive>:\`
  indicates a junction).
- **Pure-function classifier**:

  ```rust
  pub fn classify_reparse(tag: u32) -> ReparseKind;
  ```

  No I/O, no allocation. The single source of truth for tag-to-
  kind mapping. Every call site in the workspace that branches
  on a reparse tag goes through this function.
- **Buffer reader**:

  ```rust
  pub struct ReparseData {
      pub kind: ReparseKind,
      pub target: Option<PathBuf>,   // None for cloud / appexec / opaque kinds
      pub raw: Vec<u8>,              // verbatim FSCTL_GET_REPARSE_POINT buffer
  }
  pub fn read_reparse_data(path: &Path) -> io::Result<ReparseData>;
  ```

  Opens the path with `CreateFileW(... FILE_FLAG_OPEN_REPARSE_POINT
  | FILE_FLAG_BACKUP_SEMANTICS ...)`, issues
  `DeviceIoControl(FSCTL_GET_REPARSE_POINT, ...)`, parses the tag
  and substitute-name buffer, and returns the structured value.
  The raw buffer is retained so a downstream consumer can preserve
  it as an opaque xattr when an opt-in flag is added later.
- **Centralisation invariant**: there must be exactly one
  `classify_reparse` definition and exactly one `read_reparse_data`
  definition in the workspace. Existing call sites (entry build,
  walker, local-copy executor, batch replay) must call through the
  classifier before deciding on the per-kind branch.
- **Test surface for WPC-9** (#2911): for each `ReparseKind`
  variant, the regression suite must build a fixture reparse
  point of that kind on Windows using the appropriate Win32 API
  (`CreateSymbolicLinkW`, `DeviceIoControl(FSCTL_SET_REPARSE_POINT)`,
  WSL `ln -s` via `wsl.exe`, `mklink /J`, OneDrive provider stub),
  then assert that `classify_reparse(...)` returns the expected
  variant and that `read_reparse_data` surfaces the substitute
  name verbatim. Fixtures that require elevated privileges (true
  symlink creation pre-Win10 Anniversary) or third-party providers
  (OneDrive) must be `#[ignore]`-gated with a documented manual
  reproduction recipe.

## 10. Recommendations

### R1. Junctions are symlinks-for-transfer

Treat `IO_REPARSE_TAG_MOUNT_POINT` as a symlink on the sender
(matches upstream rsync on Cygwin and matches the std behaviour
already in place). On the receiver, when the source kind was
`Junction`, prefer `CreateSymbolicLinkW(... SYMBOLIC_LINK_FLAG_DIRECTORY ...)`
over the raw junction reconstruction: a symlink is functionally
equivalent for the user, requires no admin elevation when the
receiver is in Developer Mode, and round-trips across volumes
without the `\??\Volume{GUID}\` headaches. Volume mount points
(also `MOUNT_POINT` but with the volume-GUID substitute-name
form) should be skipped with a warning - reconstructing them
requires `mountvol.exe`-level privilege and is not safe to do
implicitly.

### R2. Hydrate cloud placeholders by default; expose an opt-out

The backup use case (the common case for rsync) needs the real
bytes. Continue triggering hydration by default, but emit a
one-shot INFO_GTE(NAME, 1) log line on the first dehydrated file
encountered, naming the path and the placeholder tag. Add an
opt-out flag `--preserve-cloud-placeholders` (or reuse a sensible
short-flag mnemonic) that, when set, classifies the placeholder
as a typed `Cloud(tag)` entry and preserves the raw reparse
buffer as an xattr so a same-provider destination can rematerialise
the placeholder. Default off; the flag is for mirror-mode users.

### R3. Decode WSL Linux symlinks to POSIX symlinks

`IO_REPARSE_TAG_LX_SYMLINK` carries a UTF-8 POSIX target. The
classifier should recognise it, read the target from the reparse
buffer, and route the entry through the existing symlink branch
of the file-list build with `kind = Symlink`. The wire encoding
already supports symlinks; no protocol work is needed. The
receiver on Linux gets a true POSIX symlink; the receiver on
Windows gets a true NTFS symlink (lossy: WSL-specific permission
bits are not preserved). The WSL POSIX-device tags (`AF_UNIX`,
`LX_FIFO`, `LX_CHR`, `LX_BLK`) follow the same pattern: decode the
reparse buffer and route through the existing special-file branch
(skip on non-Unix destinations).

### R4. Opaque preservation for AppExecLink and WCI

App-execution aliases and Windows Container Isolation reparse
points have provider-specific payloads that cannot be reconstructed
without the originating subsystem. Preserve them as opaque
reparse data: store the raw `FSCTL_GET_REPARSE_POINT` buffer as a
designated xattr (e.g. `user.win32.reparse`) when `-X` is active,
and emit a one-shot warning when a non-Windows destination is
detected. The default behaviour - shipping the file body that
`CreateFileW` returns - is the wrong default because the body is
provider-private bytes, not user data. A safer default is to skip
the entry with a per-path warning, leaving the receiver tree clean.

## 11. Cross-references

Internal:

- `docs/user/windows-support-matrix.md` (WPC-13, PR #4920) -
  user-facing matrix that lists this audit's symlink-and-reparse
  row as Partial pending WPC-7 / WPC-8 / WPC-9.
- `docs/audit/windows-ads-handling.md` (WPC-1) - sibling audit
  for the ADS pipeline; the WPC-8 implementation reuses the
  `xattr_windows.rs` strategy of treating Windows-specific data
  blobs as xattrs for cross-platform round-trip.
- `docs/design/windows-ads-strategy.md` (WPC-2) - parallel
  architecture pattern for binding Windows-specific metadata into
  the rsync xattr pipeline.
- `docs/windows_platform_parity.md` - the `cfg(unix)`-block
  inventory that surfaces the receiver-side `create_symlinks`
  no-op cited in section 3.2.
- `docs/audit/windows-acl-xattr-ci-matrix.md` - WAS series; the
  test-fixture approach (deterministic per-feature fixtures gated
  on Win32 availability) is the template for WPC-9.

Tracking:

- Parent: **#2869** (Windows real-world parity series).
- Sibling completed: **#4920** (WPC-13, Windows support matrix).
- Follow-up: **#2910** (WPC-8, classifier implementation),
  **#2911** (WPC-9, regression tests).
- This document: **#2909** (WPC-7).

Memory cross-links (internal): `[[project_windows_real_world_parity_unclear]]`,
`[[project_windows_parity_wip]]`.
