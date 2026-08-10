# Windows long-path (`\\?\`) support audit (WPC-5)

Tracks parent #2869 (Windows real-world parity series). Feeds
follow-up #2908 (WPC-6: implement the long-path normalisation helper
and regression-test fixtures). Cross-references the WPC-13 Windows
support matrix (`docs/user/windows-support-matrix.md`, PR #4920) and
the WPC-1 alternate data stream audit
(`docs/audit/windows-ads-handling.md`, PR #4898). Memory notes inline:
[[project_windows_real_world_parity_unclear]],
[[project_windows_parity_wip]].

## 1. Scope

WPC-5 audits oc-rsync's behaviour when source or destination paths on
Windows exceed the legacy Win32 `MAX_PATH` (260 wide chars including
the trailing NUL). The audit covers:

- The Win32 path-accepting API call sites under `crates/fast_io/src/`
  and `crates/metadata/src/`.
- The path-normalisation primitives (`to_wide`, `to_wide_path`,
  `path_to_wide`, ...) the call sites depend on, and whether any of
  them apply the `\\?\` extended-length prefix.
- The test coverage gap - both unit and integration - for paths
  longer than 260 chars on Windows.
- The application-manifest opt-in (`longPathAware`) status for the
  shipped `oc-rsync.exe`.

Out of scope: POSIX `PATH_MAX` handling on Linux/macOS (already
exercised in `crates/flist/tests/path_max_limits.rs` and
`crates/engine/src/local_copy/tests/execute_long_paths.rs`); Windows
short-name (8.3) handling; case sensitivity (tracked elsewhere in
WPC-7). Cygwin's POSIX-emulation long-path behaviour is also out of
scope because oc-rsync is a native Win32 build.

## 2. Background

Windows path-handling rules relevant to this audit:

- **`MAX_PATH = 260`**. The historical Win32 limit for an unprefixed
  path passed to ANSI or wide path-accepting APIs (`CreateFileW`,
  `FindFirstFileW`, `GetFileAttributesW`, ...). The 260 includes the
  drive letter, every separator, and the trailing NUL.
- **`\\?\` prefix lifts `MAX_PATH`**. Any path passed to a Unicode
  Win32 API that begins with `\\?\` is forwarded to the object
  manager with the parsing layer disabled. The effective limit then
  becomes `~32,767` wide chars, matching the underlying NTFS limit.
  The prefix also disables `.` / `..` normalisation, short-name
  expansion, and forward-slash translation, so callers must hand the
  kernel a canonical absolute path.
- **`\\?\UNC\` prefix for network paths**. The verbatim form of a UNC
  path replaces the leading `\\` with `\\?\UNC\`, e.g.
  `\\server\share\dir` becomes `\\?\UNC\server\share\dir`. Without
  this transformation, the bare `\\server\share\...` form is still
  subject to `MAX_PATH`.
- **`LongPathsEnabled` registry opt-in (Windows 10 1607+)**. Setting
  `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled`
  to `1` lifts `MAX_PATH` for processes that ship an application
  manifest declaring `<longPathAware>true</longPathAware>`. Without
  the manifest, the registry switch has no effect on the process.
- **NTFS itself is `~32,767`-char wide**. The 260 limit lives in
  the Win32 path-parsing layer, not the filesystem driver. Paths
  reachable only via `\\?\` (e.g. very deep trees or long file
  names) still exist on disk and are observable to processes that
  cooperate with the extended-length form.
- **`GetFinalPathNameByHandleW` returns extended form by default**.
  With `VOLUME_NAME_DOS | FILE_NAME_NORMALIZED`, the kernel hands
  back a `\\?\C:\...` style path that round-trips through
  `CreateFileW`. This is a useful escape hatch for code that already
  holds a handle but needs to reopen the same file by path.

## 3. Inventory of Windows path API call sites

All locations are `file:line` references in the repository as of the
audited tree. Each row records whether the caller transforms the
input path before the Win32 call. "No" means the path is encoded as
UTF-16 via `OsStr::encode_wide` and passed verbatim to the kernel
with no `\\?\` prefix.

### 3.1 `fast_io` crate

| Call site | API | Path source | `\\?\`-prefixed? |
|-----------|-----|-------------|------------------|
| `crates/fast_io/src/iocp/file_reader.rs:46` | `CreateFileW` (OPEN_EXISTING, FILE_GENERIC_READ) | user-supplied `&Path` via `to_wide_path` (line 315) | No |
| `crates/fast_io/src/iocp/file_writer.rs:84` | `CreateFileW` (configurable disposition, FILE_GENERIC_WRITE) | user-supplied `&Path` via `super::file_reader::to_wide_path` | No |
| `crates/fast_io/src/iocp/file_factory.rs:391,410` | `GetFinalPathNameByHandleW` (handle -> path recovery for reopen) | handle of an already-open file; returns `\\?\C:\...` form | N/A (handle in, extended form out) |
| `crates/fast_io/src/platform_copy/dispatch.rs:370,396,582,607` | `CreateFileW` (ReFS reflink + partial reflink: source and destination) | user-supplied source and destination `&Path`; ad-hoc `encode_wide` chains inline (lines 360-364, 385-389, 574-578, 597-601) | No |
| `crates/fast_io/src/platform_copy/dispatch.rs:330,551` | `GetDiskFreeSpaceW` (cluster-size probe on `dst.ancestors().last()`) | user-supplied destination, volume root only | No |
| `crates/fast_io/src/refs_detect.rs:126` | `GetVolumePathNameW` (volume root extraction) | user-supplied `&Path` via local `to_wide` (line 209) | No |
| `crates/fast_io/src/refs_detect.rs:156` | `CreateFileW` (FILE_FLAG_BACKUP_SEMANTICS to open volume root directory) | output of `GetVolumePathNameW`; always a short volume root path | No (volume roots are short by construction) |

### 3.2 `metadata` crate

| Call site | API | Path source | `\\?\`-prefixed? |
|-----------|-----|-------------|------------------|
| `crates/metadata/src/xattr_windows.rs:203` | `FindFirstStreamW` | user-supplied `&Path` via `path_to_wide` (line 77) | No |
| `crates/metadata/src/xattr_windows.rs:236` | `FindNextStreamW` | handle | N/A |
| `crates/metadata/src/xattr_windows.rs:268,312` | `CreateFileW` (stream `path:name:$DATA`, read and write paths) | user-supplied `&Path` extended with `:name:$DATA` via `stream_path_wide` (line 86) | No |
| `crates/metadata/src/xattr_windows.rs:348` | `DeleteFileW` (stream removal) | user-supplied `&Path` via `stream_path_wide` | No |
| `crates/metadata/src/acl_windows/dacl.rs:41,415` | `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW` | user-supplied `&Path` via `to_wide` in `acl_windows/common.rs:42` | No |
| `crates/metadata/src/acl_windows/sddl.rs:87,269` | `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW` (SDDL backend) | user-supplied `&Path` via `to_wide` in `acl_windows/common.rs:42` | No |

### 3.3 `platform` crate (name resolution)

| Call site | API | Path source | `\\?\`-prefixed? |
|-----------|-----|-------------|------------------|
| `crates/platform/src/name_resolution.rs:40,61` | `LookupAccountNameW` (SID lookup) | account-name string, not a filesystem path | N/A |
| `crates/platform/src/privilege.rs:191` | `LogonUserW` (impersonation flow) | username/domain strings, not a filesystem path | N/A |
| `crates/metadata/src/acl_windows/dacl.rs:453,471` | `LookupAccountNameW` (SID lookup) | account-name string | N/A |

`LookupAccountNameW` and `LogonUserW` take account-name strings, not
file paths, so the `MAX_PATH` rule does not apply. They are listed
for completeness because the audit brief enumerated them.

### 3.4 Indirect call sites via `std::fs`

The receiver pipeline, delete emitter, file-list builder, and
copy executor all route through Rust's `std::fs` primitives
(`File::open`, `OpenOptions::open`, `fs::rename`, `fs::remove_file`,
`fs::create_dir_all`, `fs::symlink_metadata`, `WalkDir`, ...).

Rust's standard library on Windows does **not** automatically
prepend `\\?\` to absolute paths. `std::sys::pal::windows::fs`
encodes the path via `to_u16s` (a plain `encode_wide + null` chain)
and calls `CreateFileW` directly. Any path longer than 259 chars
hits the same `ERROR_PATH_NOT_FOUND` / `ERROR_FILENAME_EXCED_RANGE`
that the raw Win32 APIs return, unless the consuming process has
opted into long paths via manifest + registry. See finding 1 below.

Notable indirect call sites:

- `crates/engine/src/delete/emitter/fs.rs:166,174,178,182` -
  `fs::remove_file` (internally `DeleteFileW`).
- `crates/transfer/src/receiver/transfer/*` and
  `crates/engine/src/local_copy/executor/file/*` - `File::create`,
  `OpenOptions::open`, `fs::rename`.
- `crates/flist/src/...` - `WalkDir` and `fs::symlink_metadata`.

These do not show up in the grep above because they hit Rust's
standard library, but they share the same `\\?\`-less behaviour as
the direct call sites in section 3.1 and 3.2.

## 4. Path-normalisation primitives

Exhaustive search for any helper that transforms a `&Path` before
handing it to a Win32 API:

| Helper | Location | Behaviour |
|--------|----------|-----------|
| `to_wide_path(&Path) -> io::Result<Vec<u16>>` | `crates/fast_io/src/iocp/file_reader.rs:315` | `encode_wide` + `chain(once(0))`. No prefixing, no validation. `pub(crate)`. |
| `to_wide(&Path) -> Vec<u16>` | `crates/fast_io/src/refs_detect.rs:209` (test-only local helper) | `encode_wide` + null. No prefixing. |
| `to_wide(path: &std::path::Path)` | `crates/fast_io/src/iocp/pump.rs:572` (test-only helper) | `encode_wide` + null. No prefixing. |
| `to_wide(path: &std::path::Path)` | `crates/fast_io/src/iocp/disk_batch/tests.rs:13` (test-only) | `encode_wide` + null. No prefixing. |
| `to_wide(&Path) -> Vec<u16>` | `crates/metadata/src/acl_windows/common.rs:42` | `encode_wide` + null. No prefixing. `pub(super)`. |
| `path_to_wide(&Path) -> Vec<u16>` | `crates/metadata/src/xattr_windows.rs:77` | `encode_wide` + null. No prefixing. |
| `stream_path_wide(&Path, &[u8]) -> io::Result<Vec<u16>>` | `crates/metadata/src/xattr_windows.rs:86` | Validates the stream-name component, then `encode_wide` + `:name:$DATA` + null. No `\\?\` prefix. |
| Inline `encode_wide` chains | `crates/fast_io/src/platform_copy/dispatch.rs:316,360,385,537,574,597` | Open-coded; no prefixing. |
| Inline `encode_wide` chain | `crates/fast_io/src/refs_detect.rs:118` (via `to_wide`) | No prefixing. |

There is **no helper anywhere in the tree that adds the `\\?\`
extended-length prefix**. There is no central long-path entry
point. Every Win32 path call site reinvents the same plain encoding.

The receiver-side file-list sanitiser (`crates/transfer/src/receiver/
file_list/sanitize.rs:65`) actively **rejects** any wire-format entry
that has a `Component::Prefix` (drive letter, UNC, `\\?\`, or
`\\.\`). That defence prevents a malicious sender from injecting a
verbatim path that escapes the destination tree; it does not affect
the receiver's own ability to address long destination paths via
`\\?\` because the prefix would be added by the local writer, not by
the wire entry.

`crates/core/src/message/source.rs:344` and
`crates/core/src/message/tests/part2.rs:128-140` normalise verbatim
disk and UNC prefixes for human-readable display (e.g.
`\\?\UNC\server\share\dir` -> `//server/share/dir`). That is a
display-only helper, not a long-path support primitive.

## 5. Existing test coverage

### 5.1 Long-path tests that are platform-portable

- `crates/flist/tests/path_max_limits.rs` - constructs nested
  directory trees up to `PATH_MAX - 256` (3840 wide chars on
  non-macOS, 768 on macOS). `cfg`-gated to use `PATH_MAX = 4096`
  for the `not(target_os = "macos")` arm, which **includes Windows**.
  The test passes on Linux because POSIX `PATH_MAX = 4096` and the
  underlying filesystem accepts long components. On Windows the
  same code path will trip `MAX_PATH = 260` long before reaching
  3840 chars because none of the underlying file-system primitives
  apply the `\\?\` prefix. There is no `#[cfg(windows)]` skip and
  no Windows-specific assertion: the test silently relies on
  POSIX-style behaviour.
- `crates/engine/src/local_copy/tests/execute_long_paths.rs` - same
  story. `PATH_MAX` is 4096 on Windows by virtue of the
  `not(target_os = "macos")` arm. The 23 tests in the file create
  deep trees and 200-byte components, all of which will exceed 260
  chars in their fully qualified form on Windows.
- `crates/engine/src/local_copy/filter_program_internal_tests.rs:194`
  - constructs a 5000-char filename to force an I/O error. Again
  cross-platform but not Windows-aware.

### 5.2 Tests that touch `\\?\` syntactically (path classification)

- `crates/cli/src/frontend/execution/file_list/tests.rs:492-498` -
  asserts that `\\?\C:\...` and `\\?\UNC\...` operands are
  classified as local rather than remote. Operand classification,
  not file I/O.
- `crates/engine/src/local_copy/operands.rs:373-380` - same assertion
  in the engine-side operand classifier.
- `crates/core/src/message/tests/part2.rs:128-140` - `\\?\` and
  `\\?\UNC\` paths are stripped to their canonical short form for
  display. Display-only.
- `crates/transfer/src/receiver/tests/errors_and_timeouts/
  sanitize_file_list.rs:182-200` - asserts that wire-format entries
  carrying a `Component::Prefix` (`\\?\`, drive letter, UNC) are
  rejected by the file-list sanitiser. Security defence, not a
  long-path I/O test.

### 5.3 Tests that exercise a `>260`-char path through a Win32 API on Windows

None. No fast_io or metadata test ever constructs a path longer than
260 chars and asserts that the underlying Win32 call succeeds.

### 5.4 Tests that exercise a UNC path through a Win32 API on Windows

None. `\\?\UNC\` appears only in operand-classification and
sanitiser tests, never in an actual file-open.

## 6. Findings

### Finding 1 - every Win32 path call site is `\\?\`-unaware

Every `CreateFileW`, `FindFirstStreamW`, `DeleteFileW`,
`GetNamedSecurityInfoW`, `GetVolumePathNameW`, and `GetDiskFreeSpaceW`
call site enumerated in section 3 receives the user-supplied path
verbatim through `OsStr::encode_wide + null`. None of them prefix
`\\?\`. None of them prefix `\\?\UNC\` for UNC inputs. Identical
behaviour holds for the indirect paths that go through `std::fs`
(`fs::remove_file`, `fs::rename`, `File::open`, ...), because Rust's
standard library is also `\\?\`-unaware on Windows.

Concretely: a user invocation such as
`oc-rsync src dst/very/long/.../path/file` will fail with
`ERROR_PATH_NOT_FOUND` (3) or `ERROR_FILENAME_EXCED_RANGE` (206) the
moment the fully qualified destination exceeds 259 wide chars,
regardless of how deep the source tree is.

### Finding 2 - no centralised `to_extended_path` helper exists

Section 4 enumerates eight `encode_wide + null` chains and zero
prefix-adding helpers. The closest thing to a path normalisation
primitive is `to_wide_path` in `crates/fast_io/src/iocp/file_reader.rs`
- and even that helper is `pub(crate)`, so the metadata, ACL, and
platform-copy crates each maintain their own private clone.

WPC-6 should introduce a single `to_extended_path(&Path) -> OsString`
helper in a new module `crates/fast_io/src/windows/path.rs` (the
directory does not exist yet) and migrate every call site in section 3
to route through it. See section 8 for the proposed shape.

### Finding 3 - no Windows-specific long-path test coverage

The 23 tests in `execute_long_paths.rs` and the 20-plus tests in
`path_max_limits.rs` test POSIX long-path behaviour. They run on
Windows CI today but they silently rely on the destination paths
fitting within `MAX_PATH`. Once the worktree's `tempfile::tempdir()`
plus the constructed depth crosses 260 chars on Windows, the assertion
that the file is copied will trip on `ERROR_PATH_NOT_FOUND`.

WPC-6 must add an explicit `#[cfg(windows)]` fixture that:

1. Constructs a destination path of `>300` chars by deep nesting.
2. Drives it through both `IocpWriter::create` and the standard
   `std::fs::File::create` fallback.
3. Asserts the file is written end-to-end with the documented
   content.
4. Covers the UNC variant with a mock share or a `subst`-mounted
   loopback drive (or skip with a clear reason if no UNC is
   reachable in CI).

### Finding 4 - no application-manifest `longPathAware` opt-in

There is no `build.rs` under `crates/cli/`, no `manifest.xml`, no
`embed_resource` invocation, and no `winres` dependency in
`Cargo.toml`. `oc-rsync.exe` ships without any Windows application
manifest at all, which means the `LongPathsEnabled` registry switch
has no effect on this process. The only long-path escape hatch
available to the binary today is the `\\?\` prefix on each Win32
call - which (per finding 1) is never applied.

WPC-6 should consider one of:

- Embed a manifest with `longPathAware` so that operators who have
  set `HKLM\...\LongPathsEnabled = 1` get long-path behaviour
  transparently for the standard `std::fs` call sites. Wire it
  via the `embed_resource` crate from `build.rs` on the
  `target_os = "windows"` arm.
- Or document explicitly that oc-rsync requires `\\?\`-prefixed
  paths and rely on the proposed `to_extended_path` helper for
  internal call sites.

The first option benefits `std::fs` callers automatically (which
helps `WalkDir`, `fs::rename`, `fs::remove_file`); the second is
strictly tighter and avoids ambient behaviour shifts based on
registry state. WPC-6 should pick one and document the choice.

### Finding 5 - boundary behaviour translates to a raw OS error

When a user passes a 300-char path today, the underlying Win32 call
returns `ERROR_PATH_NOT_FOUND` (raw OS error 3) or
`ERROR_FILENAME_EXCED_RANGE` (raw OS error 206). These propagate
out as `io::Error::from_raw_os_error(...)`. There is **no call site
in `crates/fast_io/` or `crates/metadata/` that recognises either
code and surfaces a user-actionable diagnostic** ("path exceeds
MAX_PATH and `\\?\` was not applied").

The user sees the generic
`No such file or directory (os error 3)` or
`The filename or extension is too long. (os error 206)` from the
caller chain (`io::Error::Display`). On Windows, "no such file or
directory" for a path the user can plainly see is a confusing failure
mode that masks the underlying long-path issue.

WPC-6 should add a Windows-specific error mapper that recognises
`ERROR_FILENAME_EXCED_RANGE` and `ERROR_PATH_NOT_FOUND` against a
path whose UTF-16 length exceeds 259 chars, and replaces the
default message with something like:
`"path exceeds Win32 MAX_PATH (260) and was not addressed via the
\\\\?\\ extended-length prefix"`.

## 7. WPC-6 acceptance criteria

The regression-test follow-up must validate, on a Windows host:

1. A source file at a `>300`-char path on NTFS is read end-to-end by
   both the `IocpReader` and the `std::fs::File` fallback.
2. A destination at a `>300`-char path on NTFS is written end-to-end
   by both `IocpWriter::create` and the standard `std::fs::File::create`
   fallback used by the receiver.
3. A UNC source `\\server\share\very-long-path` (synthesised via
   `subst` or a loopback share if a real one is unavailable in CI) is
   resolved through `\\?\UNC\` and read successfully.
4. When `LongPathsEnabled` is off **and** the binary has no manifest
   opt-in, oc-rsync still handles the path correctly via the
   `\\?\` prefix added by `to_extended_path`. The test must scrub
   any cached process-wide manifest state before asserting.
5. Error path: a malformed input such as `C:\foo\\?\bar\baz` (with
   `\\?\` in the middle of the path rather than at the start) is
   rejected by `to_extended_path` with a typed `InvalidInput` error
   rather than being silently forwarded to `CreateFileW`.
6. Round-trip: a path read back from `GetFinalPathNameByHandleW`
   already in `\\?\` form is passed straight through (no double
   prefixing) by `to_extended_path`. This is the path-from-handle
   recovery flow in `crates/fast_io/src/iocp/file_factory.rs:380`.
7. Display: when an error message contains a `\\?\`-prefixed path,
   the user-facing rendering strips the prefix (re-use the helper at
   `crates/core/src/message/source.rs:344`).

## 8. Recommendations

For WPC-6 implementation:

### 8.1 Introduce a single normalisation helper

Create `crates/fast_io/src/windows/path.rs` (the `windows` directory
does not exist yet under `fast_io/src/` and is the natural home,
consistent with how `dir_sandbox`, `iocp`, `io_uring` are organised)
exposing:

```rust
/// Returns an `OsString` suitable for passing to any wide Win32
/// path-accepting API, with the `\\?\` extended-length prefix
/// applied where it is needed and safe to do so.
///
/// Rules:
/// - Absolute disk paths (`C:\foo`) become `\\?\C:\foo`.
/// - UNC paths (`\\server\share\dir`) become `\\?\UNC\server\share\dir`.
/// - Paths already in `\\?\` or `\\?\UNC\` form are returned verbatim.
/// - Device-namespace paths (`\\.\pipe\...`) are returned verbatim.
/// - Relative paths are returned verbatim (the Win32 long-path opt-in
///   requires absolute paths; the caller is expected to absolutise
///   first if long-path semantics are desired).
/// - A malformed embedded `\\?\` mid-path is rejected with
///   `io::ErrorKind::InvalidInput`.
///
/// The result is `OsString` (not `String`) so arbitrary UTF-16 code
/// units, including unpaired surrogates that survive on NTFS,
/// round-trip without lossy conversion.
pub fn to_extended_path(path: &Path) -> io::Result<OsString>;

/// Inverse of [`to_extended_path`] for display: strips the `\\?\`
/// or `\\?\UNC\` prefix from a previously extended path. No-op
/// when the input is not in extended form.
pub fn from_extended_path(path: &Path) -> Cow<'_, Path>;
```

### 8.2 Migrate every Win32 call site

For each call site in section 3.1 and 3.2, replace the inline
`encode_wide + null` chain with:

```rust
let extended = to_extended_path(path)?;
let wide: Vec<u16> = extended.encode_wide().chain(once(0)).collect();
```

The migration is mechanical and per-crate. Recommended sequence:

1. `crates/fast_io/src/iocp/file_reader.rs:to_wide_path` becomes a
   thin wrapper around `to_extended_path`.
2. `crates/fast_io/src/iocp/file_writer.rs` switches to the same
   wrapper (already calls `to_wide_path` today).
3. `crates/fast_io/src/platform_copy/dispatch.rs` - inline the
   wrapper at the four `CreateFileW` and two `GetDiskFreeSpaceW`
   sites.
4. `crates/fast_io/src/refs_detect.rs:118` - the volume root probe
   is short by construction and may keep its plain encoding, but
   for uniformity route it through the helper as well.
5. `crates/metadata/src/xattr_windows.rs:path_to_wide` and
   `stream_path_wide` - the latter must prefix `\\?\` then append
   `:name:$DATA`, taking care not to insert the suffix between the
   prefix and the path body.
6. `crates/metadata/src/acl_windows/common.rs:to_wide` - same
   treatment, becoming the `\\?\`-aware helper for the two ACL
   backends in `dacl.rs` and `sddl.rs`.
7. Delete the per-module duplicates after the main helper is in
   place, leaving the helper as the single source of truth.

### 8.3 Add Windows-only regression fixtures

A new `crates/fast_io/tests/windows_long_path_io.rs` (gated on
`#[cfg(target_os = "windows")]`) covering the seven acceptance
criteria in section 7. The fixture should use `tempfile::tempdir()`
plus deep nesting to manufacture a `>300`-char path under a known-
NTFS volume, with a clear skip if the test runs on a non-NTFS temp
volume.

### 8.4 Decide on the application-manifest opt-in

WPC-6 should make an explicit ship/no-ship call on the
`longPathAware` manifest (finding 4). If shipped, wire it via
`build.rs` + `embed_resource` for the `target_os = "windows"` arm.
If skipped, document the decision in
`docs/user/windows-support-matrix.md` so operators understand that
oc-rsync handles long paths through the prefix helper rather than
the registry switch.

### 8.5 Add a long-path-aware error mapper

A small helper that wraps `io::Error::from_raw_os_error` to
recognise raw OS error 206 (`ERROR_FILENAME_EXCED_RANGE`) and raw
OS error 3 (`ERROR_PATH_NOT_FOUND`) against a path whose UTF-16
length exceeds 259 chars, replacing the default message with a
diagnostic that names the long-path failure mode (finding 5).

## 9. Cross-references

- WPC-13 Windows support matrix:
  `docs/user/windows-support-matrix.md` (PR #4920).
- WPC-1 alternate data stream handling audit:
  `docs/audit/windows-ads-handling.md` (PR #4898).
- Parent series: #2869 (Windows real-world parity).
- Follow-up: #2908 (WPC-6: implement long-path helper and tests).
- Memory notes: [[project_windows_real_world_parity_unclear]],
  [[project_windows_parity_wip]].
