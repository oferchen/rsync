# Windows Path Normalization Audit

Static analysis of how oc-rsync handles Windows path inputs at every entry
point: CLI parse, flist encode, wire bytes, flist decode, daemon path
resolution, and local-copy planning. Compares each result against upstream
rsync 3.4.1 (Cygwin build) which is the only authoritative reference.

Last updated: 2026-04-29
Tracks: task #1842

## Scope

oc-rsync targets native Win32 (the `windows-msvc` and `windows-gnu` Rust
targets), not Cygwin. Upstream rsync only ships a Cygwin build for Windows;
the Cygwin POSIX layer rewrites all `\` to `/` and exposes drives as
`/cygdrive/c/...`. oc-rsync sees the raw Win32 surface, so every layer that
upstream simply inherits from Cygwin must be implemented explicitly here.

Reviewed code paths:

- `crates/cli/` operand parsing (no Windows-specific normalization).
- `crates/engine/src/local_copy/operands.rs` source/dest classification.
- `crates/core/src/client/remote/invocation/transfer_role.rs` remote vs
  local detection.
- `crates/protocol/src/flist/entry/accessors.rs` `name_bytes()` and
  `strip_leading_slashes()`.
- `crates/protocol/src/flist/write/{mod.rs,encoding.rs}` flist encode.
- `crates/protocol/src/flist/read/{mod.rs,name.rs,extras.rs}` flist decode.
- `crates/transfer/src/sanitize_path.rs` daemon module path
  sanitization (mirror of upstream `util1.c:sanitize_path`).
- `crates/transfer/src/generator/file_list/walk.rs` relative path
  construction during file list build.
- `crates/transfer/src/symlink_safety.rs` unsafe symlink detection.
- `crates/transfer/src/receiver/file_list.rs` post-receive path cleanup.

## Path-Form Map

The table covers every form a Windows user might pass on the command line.
"Native" means oc-rsync running on `windows-msvc` (cmd.exe / PowerShell).
"Wire" assumes a Windows sender talking to a Linux receiver - the worst
case for path-separator divergence.

| Input Form | CLI Parse Result | Wire-Encoded `name` Bytes | Receiver-Decoded `name` | On-Disk Result | Upstream (Cygwin) |
|---|---|---|---|---|---|
| `C:\foo\bar` (drive-absolute) | `operand_is_remote` = false (Windows drive-letter exemption); `relative_prefix_components = 2` (drive + RootDir) | After `strip_prefix(base)` strips `C:\foo`, `name_bytes()` returns `bar` (UTF-8 of native string). Backslashes survive when there are subdirectories. | `PathBuf::from("bar")` (or `bar\baz` if multi-component) | Receiver writes `bar\baz` as a literal filename on Linux (no directory split) | Cygwin sees `/cygdrive/c/foo/bar`, sends `bar` over wire with `/` separators |
| `C:foo\bar` (drive-relative, no RootDir) | `operand_is_remote` = false; `relative_prefix_components = 1` (only the prefix) | `strip_prefix` may not match if `base` had RootDir; `name_bytes()` falls back to entire path including drive prefix `C:foo\bar` | `PathBuf::from("C:foo\\bar")` - on Linux, treated as a 12-byte filename | Drive letter and colon written verbatim into receiver tree | Cygwin: ambiguous; usually rejected with "drive letters not supported" |
| `\\server\share\foo` (UNC) | `operand_is_remote` = false. `operand_has_windows_prefix` returns true. `Path::components` yields `Prefix(VerbatimUNC)` + `RootDir` + components. `relative_prefix_components` correctly accounts for both. | After `strip_prefix("\\\\server\\share")`, name is `foo`. The prefix component itself is dropped. | `PathBuf::from("foo")` | Correct directory layout | Cygwin: `//server/share/foo`, double-leading-slash preserved by `clean_fname` `__CYGWIN__` block |
| `\\?\C:\foo\bar` (Win32 long-path, verbatim) | `operand_is_remote` = false. `Component::Prefix(VerbatimDisk)` recognized by `Path::components`. `relative_prefix_components = 2` (Prefix + RootDir). | After `strip_prefix`, name is `bar`. Verbatim prefix discarded. | `PathBuf::from("bar")` | Correct | Cygwin does not support `\\?\` syntax. |
| `\\?\UNC\server\share\foo` | `operand_is_remote` = false. `Component::Prefix(VerbatimUNC)`. `relative_prefix_components = 2`. | Name resolves to `foo` after strip_prefix. | `PathBuf::from("foo")` | Correct | n/a |
| `C:/foo/bar` (drive + forward slash) | `operand_is_remote` = false. `Path::components` accepts `/` on Windows. `relative_prefix_components = 2`. | `name_bytes()` on Windows returns UTF-8 of the *native* form, which uses `\` after `Path::push`. So name becomes `bar` (single component, no separator) for one-level relatives, `sub\file` for nested. | `PathBuf::from("bar")` or `PathBuf::from("sub\\file")` | One-level: correct. Nested: backslash preserved verbatim, breaking on Linux receiver. | Cygwin accepts mixed slashes, normalizes to `/`. |
| `/cygdrive/c/foo` | `operand_is_remote` = false (no colon). On native Win32 there is no `/cygdrive` mount, so `Path::components` treats this as a normal absolute path with `RootDir` + Normal components. | name gets relative path; `cygdrive/c/foo` survives | `PathBuf::from("cygdrive/c/foo")` | Receiver creates a literal `cygdrive/c/foo` directory tree | Cygwin translates `/cygdrive/c/foo` -> `C:\foo` at syscall layer, then sends `foo` |
| `/c/foo` (MSYS-style) | Same as `/cygdrive/...` - treated as a normal absolute path on native Win32. | `c/foo` over wire. | `c/foo` on Linux | Receiver creates `c/foo` tree | MSYS rewrites to `C:\foo`. Native rsync doesn't see `/c/...`. |
| `foo\bar\baz` (relative, backslash) | `operand_is_remote` = false; `relative_prefix_components = None`. | `name_bytes()` returns UTF-8 of native form: `foo\bar\baz`. Bytes contain `\` separators. | On Linux, `PathBuf::from("foo\\bar\\baz")` is a single 11-byte filename. | Receiver creates one literal file with backslashes in its name. Directory hierarchy is lost. | Cygwin's caller is expected to use `/`; if it sends `\`, same corruption occurs. |
| `foo/bar/baz` (relative, forward slash) | `operand_is_remote` = false. `Path::components` yields three Normal components. | `name_bytes()` returns UTF-8 of native form. After `Path::push`, internal storage uses `\` again unless the original input was unmodified. For paths that are stored verbatim (no `push`), the bytes will keep `/`. The actual encoding depends on whether the path went through `Path::join`. | Mixed; depends on history of the `PathBuf`. | Inconsistent. | Always `/` over the wire. |
| `C:\foo\trailing.` (trailing dot) | NTFS rejects file names ending in `.`; Win32 APIs strip silently. `Path::components` keeps the dot. | Stripped/preserved depending on filesystem layer beneath. | Whatever bytes were sent. | Receiver may write a filename ending in `.` on Linux (legal there). | Cygwin same as oc-rsync. |
| `CON`, `PRN`, `NUL`, `LPT1` | No special handling at any layer. The OS rejects open() at the kernel level. | If reached, bytes survive as-is. | Linux receiver creates the literal name (no reserved-name list on POSIX). | Linux side OK; Windows -> Windows fails on open. | Cygwin same: reserved-name handling is OS-level. |
| `host:path` (SSH-style remote) | `operand_is_remote` = true (no slash before `:`, not single drive letter). | Path goes through SSH transport, not flist directly. | n/a (sender is remote rsync). | n/a | Same. |
| `C:` (drive letter only, no path) | On Windows: `operand_is_remote` = false (drive-letter exemption). On Unix: treated as `host:path` with empty path (remote). | On Windows: source path is `C:` (current dir on C:). On Unix: SSH attempt to host `C`. | n/a in pure-local case | Native: opens current dir on C:. Cross-platform: SSH. | Cygwin: `C:` is not a valid path; usually rejected. |

## Findings

### F1. Backslash leaks into wire-encoded filenames (HIGH severity)

`crates/protocol/src/flist/entry/accessors.rs:120-128` defines
`name_bytes()` as:

```rust
#[cfg(not(unix))]
self.name().as_bytes()
```

`self.name()` uses `to_str().unwrap_or("")`, which on Windows returns the
native string containing `\` separators. The wire encoder
(`crates/protocol/src/flist/write/encoding.rs:79-98`) writes those bytes
verbatim. A Windows sender thus emits `subdir\file.txt` over the wire,
which an upstream-rsync receiver on Linux interprets as a single 16-byte
filename, not `subdir/file.txt`. Upstream's Cygwin build never has this
problem because the Cygwin POSIX layer presents `/`-separated paths to
rsync.

**Severity rationale.** Any Windows -> Linux push or daemon transfer with
nested directories silently corrupts the on-disk layout. This is a
correctness bug, not a performance issue.

**Recommended fix.** Add a `to_wire_path_bytes()` helper that mirrors
`to_wire_mode()` in `wire_mode.rs`: identity on Unix, replace `\` with
`/` on Windows. Apply it in `name_bytes()`, in
`write_symlink_target()` (`encoding.rs:115`), and in
`strip_leading_slashes()` (the `#[cfg(not(unix))]` branch already uses
`to_string_lossy`, but does not normalize separators). The receiver side
already accepts both via `PathBuf::from` so no change is needed there.

This fix is too large for an audit-only PR because it touches the wire
contract and requires golden-byte test updates. Filed as a follow-up.

### F2. `transfer_role::operand_is_remote` duplicates `engine::local_copy::operand_is_remote` and lacks the `\\?\` extended-prefix detection (MEDIUM severity)

`crates/core/src/client/remote/invocation/transfer_role.rs:25-56` is a
"simplified version" of `engine::local_copy::operand_is_remote`. The
local-copy version has `operand_has_windows_prefix` which recognizes
`\\?\`, `\\.\`, and bare UNC. The transfer-role version only special-cases
single drive letters.

In practice the two versions agree on every path tested in the table
above because the colon-and-slash heuristic is sufficient: any
`\\?\C:\...` form contains a colon at position 4 with `\` before it, so
`before.contains('\\')` returns true and the operand is correctly
classified as local.

But the duplication is fragile. A future regression in either copy could
diverge them. The audit recommends consolidating into a single
crate-public helper.

**Severity rationale.** No active bug, but a maintenance risk and
long-standing TODO in the comment header itself ("which is not public").

**Recommended fix.** Promote `engine::local_copy::operand_is_remote` to
`pub` (or move to a shared `core` utility) and delete the duplicate.
Filed as a follow-up.

### F3. Symlink safety check uses POSIX absolute-path detection only (LOW severity)

`crates/transfer/src/symlink_safety.rs:36-54` flags a symlink target as
unsafe if its first byte is `b'/'`. On Windows, `C:\Windows\...` would
not match this rule and would not be classified as absolute. The
`is_unsafe_symlink` function would then evaluate the target as relative,
walking `..` components and computing depth.

In the current Windows build, symlink creation generally fails earlier
because the receiver lacks the POSIX symlink primitives (the audit in
`docs/windows_platform_parity.md` notes "Symlink Safety - No-op safety
checks (always allow)"). So this is dead code on Windows today, but if
symlink support lands the gap will activate.

**Severity rationale.** Not currently exploitable because Windows
symlink creation is no-op stubbed. Becomes a real issue when Windows
symlink support is implemented.

**Recommended fix.** When Windows symlink support is added, extend
`is_unsafe_symlink` with a Windows-side helper that recognizes drive-
absolute (`X:`) and UNC (`\\`) prefixes as absolute. Filed as a follow-up
linked to the symlink-on-Windows tracker.

### F4. `name_bytes()` on non-Unix performs UTF-8 lossy round-trip (LOW severity)

`accessors.rs:120-128` falls back to `name().as_bytes()` which uses
`to_str().unwrap_or("")`. On Windows, paths that contain unpaired UTF-16
surrogates (legal in NTFS) would become an empty string. The same gap
exists in `entry/constructors.rs:152-155` where `from_raw_bytes` uses
`String::from_utf8_lossy`.

oc-rsync currently has no Windows-only test fixture that exercises
non-UTF-8 file names, so this code path is untested. Upstream rsync's
Cygwin build sees only UTF-8 names because Cygwin enforces UTF-8 on
filesystems by default.

**Severity rationale.** Affects only files with non-UTF-8 names, which
are extremely rare on Windows. Existing behavior matches Cygwin in
practice.

**Recommended fix.** Use `OsStrExt::encode_wide` on Windows to convert to
WTF-8 (a superset of UTF-8 that admits unpaired surrogates) and round-
trip via `OsString::from_wide` on the receive side. This is a future
hardening pass; not warranted today.

### F5. `strip_leading_slashes` uses `to_string_lossy` on non-Unix (LOW severity)

`accessors.rs:94-106`, the `#[cfg(not(unix))]` branch, calls
`to_string_lossy()` which can lose information for non-UTF-8 names. The
function only ever drops `/` characters, so the operation could be done
losslessly via `OsString::push` and prefix slicing.

**Severity rationale.** Same population as F4: non-UTF-8 Windows
filenames. Also, `--relative` mode on Windows is rare because absolute
paths there embed drive letters, which would already have been rejected
upstream of this point.

**Recommended fix.** Same direction as F4; switch to `encode_wide` and
slice on UTF-16 code units. Future hardening.

### F6. Wire transport does not normalize trailing dots, spaces, or DOS reserved names (INFORMATIONAL)

NTFS rejects filenames ending with `.` or ` ` and reserved names
(`CON`, `PRN`, `NUL`, `LPT1-9`, `COM1-9`, `AUX`). oc-rsync forwards such
names verbatim. Linux side accepts them; Windows -> Windows fails at
open() time.

Upstream rsync has the same behavior. No action recommended; this is
documented for completeness so future contributors do not invent custom
rules that diverge from upstream.

## In-PR Fix

This PR is documentation-only. The high-severity Finding F1 is too
broad for an audit-style PR (it requires a wire-format change with
golden-byte updates and dedicated regression tests on Linux receivers
testing Windows-style inputs). No code is modified.

A regression test that demonstrates F1 - encoding a `FileEntry` whose
path was built via `PathBuf::push` on Windows and asserting the wire
bytes contain only `/` separators - is queued for the follow-up fix.

## Follow-up Tasks

1. **HIGH** Implement `to_wire_path_bytes()` and apply at every wire
   encode site. Add golden-byte test for nested-directory entries on
   Windows. Tracks F1.
2. **MEDIUM** Promote `engine::local_copy::operand_is_remote` to a
   shared helper and delete the duplicate in `transfer_role.rs`. Tracks
   F2.
3. **LOW** When Windows symlink support is implemented, extend
   `is_unsafe_symlink` to recognize Windows-absolute paths. Tracks F3.
4. **LOW** Migrate `name_bytes()`, `from_raw_bytes()`, and
   `strip_leading_slashes()` non-Unix branches to WTF-8 / UTF-16-aware
   conversion. Tracks F4 and F5.

## References

- Upstream `util1.c:943-1011` (`clean_fname`) and `util1.c:1035-1108`
  (`sanitize_path`).
- Upstream `flist.c:1245` (`thisname` copy), `flist.c:1567` and
  `flist.c:2017` (`f_name(file, fbuf)`).
- Upstream `util1.c:955-961` `__CYGWIN__` two-leading-slash preservation.
- oc-rsync `crates/transfer/src/sanitize_path.rs` (mirror of
  `sanitize_path`).
- oc-rsync `crates/protocol/src/flist/wire_mode.rs` (precedent for
  identity-on-Unix / normalize-on-Windows wire helpers).
- oc-rsync `crates/engine/src/local_copy/operands.rs` (the better
  Windows-prefix detection, lines 189-360).
- Rust `std::path::Component::Prefix` and the `windows-msvc`
  documentation on path separators.
