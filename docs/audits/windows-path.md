# Windows Path Audit: Backslash Leak in Wire-Encoded Filenames

Tracker: oc-rsync task #1939 ("Document the High-severity backslash-leak
finding with reproduction steps and the remediation plan referencing task
#1905").

Parent audit: task #1842 - completed audit
[`docs/audits/windows-path-normalization.md`](windows-path-normalization.md).
Fix tracker: task #1905 - the wire-encoding fix delivered through commits
referenced in [`docs/audits/windows-path-separator-encoding.md`](windows-path-separator-encoding.md).

Companion edge-case survey: [`docs/audits/windows-path-edge-cases.md`](windows-path-edge-cases.md).

Last updated: 2026-05-01.

## 1. Headline finding

When oc-rsync runs on a native Win32 target (`windows-msvc` or
`windows-gnu`) and pushes nested directories, the sender emits filename
bytes containing literal `\` (0x5C) separators on the wire. Upstream rsync
3.4.1 and any POSIX-native peer interpret those bytes as part of a single
filename (since `\` is a legal POSIX filename byte), not as a directory
separator. The result is silent on-disk corruption: a file the Windows
side thinks is `subdir/file.txt` lands as a literal file named
`subdir\file.txt` on the receiver.

## 2. Severity rating - HIGH

The classification is HIGH for three independent reasons:

1. **Silent data corruption.** No error is raised on either side. The
   Windows sender encodes bytes that look syntactically valid; the POSIX
   receiver writes a literal filename in the destination root. The user
   sees "transfer succeeded" and only discovers the corruption when they
   walk the destination tree and find collapsed names.
2. **Cross-stack interop break.** A Windows oc-rsync sender talking to
   upstream rsync (Cygwin or POSIX-native) cannot interoperate. Upstream
   has no normalization in `flist.c:send_file_entry()` (the Cygwin POSIX
   layer normalizes one level higher), so a non-Cygwin native Windows
   peer is the first time the rsync wire contract is tested with `\`
   bytes.
3. **No collision with destructive operations is required.** Even a
   read-only mirror push corrupts the destination tree on the first
   transfer. There is no recovery short of rerunning the entire transfer
   after the fix lands.

The severity matches finding F1 of the parent audit
`windows-path-normalization.md` (HIGH; "Backslash leaks into wire-encoded
filenames").

## 3. Affected code paths

Each site listed below is a place where a `Path` or `PathBuf` becomes
wire bytes (or feeds a filter rule) without separator normalization. The
list is exhaustive as of master prior to the #1905 fix landing. After
that fix, every site cited routes through
`crates/protocol/src/flist/wire_path.rs::path_bytes_to_wire`, which
performs the `\` -> `/` rewrite on Windows.

### 3.1 Wire emission sites (sender)

| Site | What converts | Defect |
|---|---|---|
| `crates/protocol/src/flist/entry/accessors.rs:131-135` `name_bytes()` | Returned the underlying `OsStr` bytes verbatim on `#[cfg(not(unix))]`. | A `PathBuf` built via `Path::push("subdir"); push("file.txt")` on Windows produced bytes `subdir\file.txt`. |
| `crates/protocol/src/flist/write/mod.rs:376-384` `write_entry()` | Calls `entry.name_bytes()`, optionally runs iconv (byte-transparent), writes via `write_name()`. | Inherited the leak from `name_bytes()`. |
| `crates/protocol/src/flist/write/encoding.rs:106-127` `write_symlink_target()` | Encoded `target.as_os_str().as_encoded_bytes()` directly. | Symlink targets containing `\` separators were written verbatim. |
| `crates/transfer/src/generator/file_list/walk.rs:60` relative path | `relative = path.strip_prefix(base)` produced a `PathBuf` containing `\` on Windows. | Stored verbatim in `FileEntry::name`, then re-emitted via `name_bytes()`. |
| `crates/batch/src/writer.rs` batch records | Write the wire stream verbatim. | Inherited any sender leak; replaying a batch file built on Windows reproduces F1. |

### 3.2 Filter-evaluation sites (filter-match)

| Site | What converts | Defect |
|---|---|---|
| `crates/transfer/src/generator/file_list/walk.rs:106` `filter_chain.allows(&relative, ...)` | The `relative` `PathBuf` carried `\` on Windows, but filter patterns are `/`-anchored. | Anchored rules like `/build/*` failed to match `build\out.o`; descendant rules silently bypassed. |
| `crates/transfer/src/receiver/directory/deletion.rs:135-138` `filter_chain.allows_deletion(&rel_for_filter, ...)` | `rel_for_filter = dir_relative.join(&name)` uses the platform separator. | Same pattern as walk.rs, but on the receiver side during `--delete` walks. |
| `crates/filters/src/compiled/rule.rs:38-76` `CompiledRule::matches()` | `globset::GlobMatcher::is_match(path)` on a `Path`. | The matchers were compiled with `literal_separator(true)`. globset 0.4 on Windows does not rewrite `\` to `/` before anchored matching. |

The filter sites are listed because they share a remediation: any helper
that normalizes a `Path` to a `/`-separated wire form must be applied
before both flist emit and filter evaluation. The parent audit catalogues
these as F2 ("filter-rule matching against `\`-containing relative
paths").

### 3.3 Symlink-safety check (latent)

`crates/transfer/src/symlink_safety.rs::is_unsafe_symlink` walks bytes
treating `/` as the only segment separator (mirroring upstream
`generator.c::unsafe_symlink`). On Windows, a target obtained via
`std::fs::read_link()` may carry `\`. Today this site is dormant because
Windows symlink creation/preservation is a no-op in the receiver path
(see `docs/windows_platform_parity.md`); it activates if Windows
symlink support lands without separator normalization at the read_link
call site.

### 3.4 Sites that are NOT defective

For completeness, the audit explicitly reasoned through these and
confirmed no leakage:

- `crates/protocol/src/flist/read/name.rs:35-99` `read_name()` and
  `clean_and_validate_name()` (the receiver side): these intentionally
  treat only `b'/'` as separator, mirroring upstream
  `target/interop/upstream-src/rsync-3.4.1/util1.c:943-1011`
  `clean_fname()`. They assume the wire-form invariant; correct.
- `crates/transfer/src/sanitize_path.rs::sanitize_path()`: byte-level
  `/`-only processing, mirrors upstream
  `target/interop/upstream-src/rsync-3.4.1/util1.c:1035-1108`. Correct.
- `crates/cli/src/frontend/arguments/parser/mod.rs` `to_string_lossy()`
  calls: operate on user-supplied operands, not on `PathBuf`s after
  directory traversal. Operands become local syscall arguments after
  `operand_is_remote()` classification; they never become flist
  filenames.
- `crates/core/src/client/remote/invocation/transfer_role.rs:25-56`
  `operand_is_remote()`: reads `\` to decide local vs remote (a
  classification, not a re-emission).
- `crates/engine/src/local_copy/operands.rs` Windows-prefix detection
  (`\\?\`, `\\.\`, UNC): used only to compute
  `relative_prefix_components` (a count, not a string).
- iconv encoding conversion at
  `crates/protocol/src/flist/write/encoding.rs` operates on opaque bytes.
- xattr names are not paths.

## 4. Reproduction steps

### 4.1 Setup (Windows-msvc sender, Linux receiver)

```cmd
:: On a Windows host, build oc-rsync against `windows-msvc`:
cargo build --release --target x86_64-pc-windows-msvc

:: Create a nested source tree:
mkdir C:\tmp\src\subdir
echo hello > C:\tmp\src\subdir\file.txt
echo top   > C:\tmp\src\root.txt
```

### 4.2 Trigger over a daemon transfer

On the Linux receiver host (running upstream rsync 3.4.1 daemon or
oc-rsync daemon - both reproduce):

```sh
# Configure /etc/rsyncd.conf with a writable module:
# [push]
# path = /tmp/dst
# read only = false
sudo rsync --daemon
```

On the Windows host:

```cmd
oc-rsync -av C:\tmp\src\ rsync://linux-host/push/dst/
```

### 4.3 Observe the corruption

On the Linux receiver:

```sh
$ ls -la /tmp/dst
total 16
drwxr-xr-x 2 user user 4096 May  1 12:00 .
drwxr-xr-x 4 user user 4096 May  1 12:00 ..
-rw-r--r-- 1 user user    4 May  1 12:00 root.txt
-rw-r--r-- 1 user user    6 May  1 12:00 subdir\file.txt   # <-- LITERAL BACKSLASH IN NAME
```

Expected layout (correct):

```sh
/tmp/dst/
  root.txt
  subdir/
    file.txt
```

### 4.4 Capture the wire bytes for confirmation

Use the batch-mode capture harness rather than packet sniffing - it
preserves the full file-list encoding without TLS or daemon framing
noise:

```cmd
oc-rsync -av --write-batch=C:\tmp\batch.bin C:\tmp\src\ rsync://linux/push/dst/
```

Then inspect the batch file on a POSIX host:

```sh
$ xxd /tmp/batch.bin | grep -A1 "subdir"
... 73 75 62 64 69 72 5c 66 69 6c 65 2e 74 78 74 ...
       s  u  b  d  i  r  \  f  i  l  e  .  t  x  t
```

The `5c` byte (literal `\`) appears between `subdir` and `file.txt`
where the wire contract requires `2f` (literal `/`).

### 4.5 Compare with upstream Cygwin rsync

```sh
# On Cygwin running on the same Windows host:
rsync -av --write-batch=/cygdrive/c/tmp/cygwin-batch.bin \
    /cygdrive/c/tmp/src/ rsync://linux/push/dst/

$ xxd /cygdrive/c/tmp/cygwin-batch.bin | grep -A1 "subdir"
... 73 75 62 64 69 72 2f 66 69 6c 65 2e 74 78 74 ...
       s  u  b  d  i  r  /  f  i  l  e  .  t  x  t
```

The Cygwin build emits `2f` (forward slash) because the Cygwin POSIX
layer presents `/`-separated paths to the rsync application; rsync
itself never sees a `\`.

### 4.6 Local-copy reproduction (no daemon)

The same code paths run for local copies. Even on a single Windows host,
the bug manifests if you ever serialize the file list (batch mode,
`--server` over local SSH, or `oc-rsync --debug=flist`):

```cmd
oc-rsync -av --debug=flist C:\tmp\src\ C:\tmp\dst\
```

The debug log emits `subdir\file.txt` for the relative name where
upstream Cygwin rsync would log `subdir/file.txt`.

## 5. Why upstream does not have this issue

Upstream rsync 3.4.1 has no native Windows port. The sole supported
Windows build is Cygwin, whose POSIX layer rewrites every `\` to `/` and
exposes drive letters as `/cygdrive/c/...` before the application sees
them. By the time
`target/interop/upstream-src/rsync-3.4.1/flist.c:534-570`
`send_file_entry()` writes filename bytes, those bytes are already
`/`-separated. The only `\` handling in upstream lives in the Cygwin
guard at
`target/interop/upstream-src/rsync-3.4.1/util1.c:955-961` and only
strips trailing-`\` artifacts of the Cygwin path layer; `flist.c` itself
has no separator awareness.

oc-rsync targets native Win32 directly, bypassing the Cygwin POSIX
boundary. It is therefore the first rsync implementation that has to
perform separator normalization in user space. This is a new failure
mode that upstream's design did not need to consider, and it is exactly
why the parent audit (#1842) flagged it as HIGH severity.

## 6. Remediation plan (issue #1905)

The remediation introduces a single helper at the path-to-wire boundary
and threads it through every emit site enumerated in section 3.

### 6.1 Helper API

```rust
// crates/protocol/src/flist/wire_path.rs

/// Returns the wire-format byte representation of a filesystem path.
///
/// On Unix, this is a zero-copy borrow of the path's `OsStr` bytes.
/// On Windows, `\` separators are translated to `/` so the bytes
/// match the format a POSIX peer expects on the wire. Allocation
/// is avoided when the path contains no `\` byte.
pub(crate) fn path_bytes_to_wire(p: &Path) -> Cow<'_, [u8]>;
```

The signature mirrors the precedent set by
`crates/protocol/src/flist/wire_mode.rs` (identity-on-Unix, normalize on
non-Unix). The helper allocates only when the input contains at least
one `\` byte, so `/`-only inputs (the common case after the fix lands)
take the borrow path with no allocation.

### 6.2 Sites to wire through the helper

1. `crates/protocol/src/flist/entry/accessors.rs:131-135` -
   `name_bytes()` returns `path_bytes_to_wire(&self.name)`. This single
   call covers every flist-emit caller transitively, since
   `crates/protocol/src/flist/write/mod.rs:376` reads `name_bytes()`.
2. `crates/protocol/src/flist/write/encoding.rs:106-127` -
   `write_symlink_target()` runs `path_bytes_to_wire(target.as_path())`
   before `writer.write_all(&target_bytes)`.
3. `crates/transfer/src/generator/file_list/walk.rs:60` and
   `crates/transfer/src/generator/file_list/walk.rs:106` - normalize
   `relative` to a `/`-separated `PathBuf` so both `create_entry` and
   `filter_chain.allows` see wire-canonical bytes. This collapses F1 and
   F2 of the parent audit into one fix point.
4. `crates/transfer/src/receiver/directory/deletion.rs:135` - normalize
   `rel_for_filter` before the `allows_deletion` call.

### 6.3 Inverse direction (receiver decode)

A symmetric helper is **not strictly required** because:

- `crates/protocol/src/flist/read/name.rs::clean_and_validate_name`
  already enforces the wire contract: only `b'/'` is treated as a
  separator. Wire bytes the receiver decodes are mapped through
  `PathBuf::from(&str_lossy)` which on Windows accepts both `/` and `\`
  during `Path::join`, so re-rooting under the destination directory
  works transparently.
- However, a `wire_to_path(bytes: &[u8]) -> Cow<'_, Path>` helper is
  recommended for symmetry and for any future code that needs to
  reconstruct an explicit Win32 form (e.g. for `\\?\`-prefixed long-path
  opens). On POSIX it is identity; on Windows it can reuse the standard
  library's path-join semantics.

### 6.4 Regression test (mandatory acceptance criterion)

A property test that builds a `FileEntry` whose `name` was constructed
via `PathBuf::push("subdir"); push("file.txt")` on Windows must assert
the wire bytes contain only `/` (no `0x5C`). A symmetric round-trip test
must decode those bytes through the receiver path and confirm the
resulting `PathBuf::iter()` yields exactly `["subdir", "file.txt"]`.

The fix work has landed via commits referenced in
`docs/audits/windows-path-separator-encoding.md`; that audit confirms
each site in section 3 now routes through `path_bytes_to_wire`. The
existence of `crates/protocol/src/flist/wire_path.rs` with the test
suite at lines 62-137 covers the regression-test obligation.

### 6.5 CI matrix entry

A Windows-sender -> Linux-receiver interop job is required to keep
future regressions visible. The companion audit
`docs/audits/windows-path-edge-cases.md` lists this as a follow-up
under the "Long paths > 260 without `\\?\`" and "F1 closure" rows. The
job exercises:

- `oc-rsync` on `windows-msvc` and `windows-gnu` as sender.
- Upstream rsync 3.4.1 on Linux as receiver.
- A nested source tree (depth >= 3) with mixed `/` and `\` operands.
- An assertion that the destination layout matches the source layout
  byte-for-byte, including filenames that themselves contain a `\`
  byte (which is legal on POSIX and must round-trip even after the
  separator-normalization fix - that fix only rewrites separator bytes
  introduced by `Path::join`, not user-chosen filename bytes).

The asymmetry above is the subtlest part of the fix: a filename like
`weird\name` typed on POSIX must reach Windows without modification,
because `\` is a legal filename byte on POSIX. The Windows-side
remediation only rewrites the bytes introduced by the Win32
`Path::push` operation, which the OS itself created - never bytes that
came from a peer. The helper's `#[cfg(unix)]` branch therefore returns
`Cow::Borrowed(p.as_os_str().as_bytes())` unconditionally, preserving
filename `\` bytes verbatim on POSIX senders.

## 7. Test gaps

Existing tests that should have caught this defect but did not:

- `crates/protocol/src/flist/write/tests.rs` - covers wire-byte
  roundtrips but uses POSIX-only fixtures. No `#[cfg(windows)]` test
  case constructed a `PathBuf` via `push` and asserted on the emitted
  bytes.
- `crates/transfer/src/generator/file_list/tests.rs` (and sibling test
  modules) - all path-construction tests use `/`-only literals, which
  on Windows happen to take the borrow path through `Path::components`
  and never exercise the `Path::push` path that produces `\` separators.
- `crates/filters/tests/` - filter-rule tests use `&str` patterns and
  `&str` paths; they never feed a `PathBuf` constructed with platform
  separators into `CompiledRule::matches`.
- `crates/protocol/tests/golden/` - the golden-byte tests for flist
  encoding were authored on POSIX hosts. There is no Windows-target
  golden run in CI, so the difference would not show up as a golden
  diff.

The root cause is that the CI matrix exercises `windows-msvc` only as a
*build* target plus a subset of platform-agnostic unit tests; no
sender-emit golden test runs on Windows. The remediation includes a
Windows-host CI job (see section 6.5) that loads a precomputed
golden-byte file and asserts on the emitted wire stream. After #1905
landed, `crates/protocol/src/flist/wire_path.rs:106-136` adds the
`#[cfg(windows)]` test cases that exercise `PathBuf::push` and
mixed-separator inputs.

## 8. Related audits and tracker entries

- **#1842** (closed) - parent audit
  `docs/audits/windows-path-normalization.md`; surfaced this defect as
  finding F1 and catalogued the broader path-form parsing surface.
- **#1905** (the fix tracker referenced by this audit) - delivered the
  `path_bytes_to_wire` helper and wired it through every site in
  section 3. Verification documented in
  `docs/audits/windows-path-separator-encoding.md`.
- **#1866** - Windows ACL preservation. Adjacent in the Windows-platform
  parity work; not directly affected by separator normalization but
  shares the same `windows_platform_parity.md` followup matrix.
- **#1867** - Windows xattr preservation. Same context as #1866.
- **#1869** - Windows ACL/xattr CI matrix entry, audited at
  `docs/audits/windows-acl-xattr-ci-matrix.md`. Should be expanded to
  include the Windows-sender path-normalization regression test.
- **#1900** - IOCP CI matrix entry, audited at
  `docs/audits/windows-iocp-benchmark.md`. Independent of this defect
  but part of the same "first-class Windows support" workstream.

## 9. References

### Upstream rsync (`target/interop/upstream-src/rsync-3.4.1/`)

- `flist.c:534-570` `send_file_entry()` - filename emission. Bytes are
  written verbatim; no separator translation in the application layer.
- `flist.c:701-768` `recv_file_entry()` - the `thisname` buffer; treats
  every byte after prefix-decompression as either `/` or part of a
  component name.
- `util1.c:943-1011` `clean_fname()` - canonical wire-form path
  cleanup; only `b'/'` is treated as a separator.
- `util1.c:955-961` `__CYGWIN__` block - the only `\` handling in
  upstream, scoped to the Cygwin POSIX boundary.
- `util1.c:1035-1108` `sanitize_path()` - byte-level `/`-only
  processing for daemon module roots.
- `exclude.c:1031-1108` `check_filter()` - filter-rule evaluation
  against `/`-separated wire-form `fname`.

### oc-rsync sources

- Helper: `crates/protocol/src/flist/wire_path.rs` (introduced by
  #1905).
- Sender path: `crates/protocol/src/flist/entry/accessors.rs:131-135`
  (`name_bytes()`),
  `crates/protocol/src/flist/write/mod.rs:376-384`
  (`write_entry()`),
  `crates/protocol/src/flist/write/encoding.rs:106-127`
  (`write_symlink_target()`).
- Walk and filter: `crates/transfer/src/generator/file_list/walk.rs:60,
  106`,
  `crates/transfer/src/receiver/directory/deletion.rs:135`.
- Receiver path:
  `crates/protocol/src/flist/read/name.rs:35-234`,
  `crates/transfer/src/sanitize_path.rs`.
- Existing related audits: `docs/audits/windows-path-normalization.md`,
  `docs/audits/windows-path-separator-encoding.md`,
  `docs/audits/windows-path-edge-cases.md`,
  `docs/windows_platform_parity.md`.

### Rust standard library

- `std::path::Path` - opaque cross-platform path type; `Path::push` and
  `Path::join` use the platform separator.
- `std::path::MAIN_SEPARATOR` - `'/'` on Unix, `'\'` on Windows.
- `std::os::unix::ffi::OsStrExt::as_bytes` - zero-copy `OsStr` -> `&[u8]`
  on Unix.
- `std::ffi::OsStr::as_encoded_bytes` - WTF-8 view of the underlying
  storage; on Windows this preserves `\` bytes as `0x5C`.

### Cygwin path layer

- Cygwin path translation overview:
  https://cygwin.com/cygwin-ug-net/using.html#using-pathnames
- Cygwin POSIX-to-Win32 mapping:
  `winsup/cygwin/path.cc` in the Cygwin source tree (the canonical
  reference for how `\` becomes `/` before the application sees it).
