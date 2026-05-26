# Long-path regression test spec (WPC-6)

Tracks parent #2869 (Windows real-world parity series). Follows WPC-5
(`docs/audit/windows-long-path-support.md`). Implements the test-fixture
portion of #2908 (WPC-6: long-path normalisation helper + regression
tests).

## 1. Objective

Provide comprehensive regression coverage for Windows paths exceeding
the legacy Win32 `MAX_PATH` (260 wide chars). The tests validate that
the `to_extended_path` helper introduced by WPC-6 correctly prefixes
long paths with `\\?\` (or `\\?\UNC\`) and that every file-system
operation oc-rsync performs succeeds end-to-end at these lengths.

All tests in this spec are `#[cfg(target_os = "windows")]` and run
exclusively on the Windows CI matrix leg.

## 2. Test scenarios by path length

| ID | Total path length (wide chars) | Purpose |
|----|-------------------------------|---------|
| W1 | 260 | Boundary - last char that fits without `\\?\` |
| W2 | 261 | First char that requires `\\?\` |
| W3 | 300 | Common overshoot from deep project trees |
| W4 | 500 | Moderate nesting with long directory names |
| W5 | 1000 | Deep hierarchy with short names (100+ levels) |
| W6 | 32000 | Near NTFS absolute ceiling (32767 - overhead) |

Lengths are measured as `fully_qualified_path.encode_wide().count()`
including drive letter, separators, and filename - but excluding the
trailing NUL.

### 2.1 Nested-directory composition

For scenarios W4-W6, paths are built by nesting directories rather than
using a single extremely long component. This reflects real-world usage
(e.g. `node_modules` trees, deeply nested build artifacts) and exercises
the recursive `create_dir_all` path.

Component length stays within NTFS's 255-char per-component limit.
Each test calculates how many levels of nesting are needed to hit the
target total length, then creates a file at the leaf.

## 3. File operations to verify

Each path-length scenario exercises the following operations through
`to_extended_path`:

| Operation | Win32 API exercised | oc-rsync call site |
|-----------|--------------------|--------------------|
| Create directory tree | `CreateDirectoryW` (via `std::fs::create_dir_all`) | receiver, engine |
| Create file | `CreateFileW` (CREATE_NEW) | `IocpWriter::create`, `File::create` |
| Write content | `WriteFile` | `IocpWriter::write`, `File::write_all` |
| Read content | `ReadFile` + `CreateFileW` (OPEN_EXISTING) | `IocpReader::open`, `File::open` |
| Stat / metadata | `GetFileAttributesExW` (via `std::fs::metadata`) | flist builder, quick-check |
| Rename | `MoveFileExW` (via `std::fs::rename`) | temp-file commit |
| Delete file | `DeleteFileW` (via `std::fs::remove_file`) | delete emitter |
| Delete directory | `RemoveDirectoryW` (via `std::fs::remove_dir`) | delete emitter |
| Symlink (file) | `CreateSymbolicLinkW` | symlink transfer |
| Symlink (directory) | `CreateSymbolicLinkW` + DIRECTORY flag | directory symlink transfer |
| ACL get/set | `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW` | metadata -A mode |
| Xattr (ADS) read/write | `CreateFileW` (stream path `:name:$DATA`) | metadata -X mode |

### 3.1 Assertion per operation

Each operation asserts:

1. No `io::Error` returned.
2. The operation's observable effect is present (file exists with
   expected content, directory exists, symlink target resolves, ACL
   entries match, ADS content matches).
3. The path returned by `GetFinalPathNameByHandleW` round-trips through
   `from_extended_path` to the original user-supplied path.

## 4. Round-trip transfer verification

Beyond isolated API calls, two end-to-end tests validate the full
oc-rsync transfer pipeline:

### 4.1 Push with long destination paths

1. Create a source tree with paths at 300, 500, and 1000 chars.
2. Push (`oc-rsync src/ dst/`) where `dst/` is chosen so the fully
   qualified destination exceeds `MAX_PATH` for every file.
3. Assert every file in `dst/` has identical content to the source.
4. Assert file count, directory count, and symlink count match.

### 4.2 Pull with long source paths

1. Create a destination tree with paths at 300, 500, and 1000 chars.
2. Pull from that tree to a short-named destination.
3. Assert content parity.
4. Assert that the pulled files retain correct metadata (mtime, perms).

### 4.3 Bidirectional round-trip

1. Push a tree containing a 500-char path to a remote destination.
2. Pull it back to a different local directory.
3. Assert byte-for-byte content equality and metadata parity.

## 5. Edge cases

### 5.1 Unicode characters in long paths

- Test with CJK characters (3-byte UTF-8, 1 wide char each) to verify
  that length accounting uses wide-char count, not byte count.
- Test with emoji (4-byte UTF-8, surrogate pair = 2 wide chars) to
  ensure surrogate pairs do not split across the 32767 boundary.
- Test with combining characters (e.g. `e` + combining acute) to verify
  no NFC/NFD normalisation is applied by the helper.

### 5.2 NTFS trailing-dot and trailing-space quirks

NTFS silently strips trailing dots and spaces from path components
unless addressed via `\\?\`. Tests must verify:

- A file named `data .txt` (trailing space) is preserved verbatim when
  created through `to_extended_path`.
- A directory named `build...` (trailing dots) is preserved verbatim.
- Round-trip: content written to `dir./file .dat` is readable at exactly
  that name (no stripping).

### 5.3 Path component at NTFS maximum (255 chars)

- A single filename of exactly 255 wide chars within a long path.
- A filename of 256 wide chars is rejected with a clear error (NTFS
  component limit, independent of total-path limit).

### 5.4 Paths already in extended form

- A path supplied as `\\?\C:\very\long\...` is not double-prefixed.
- A UNC path supplied as `\\?\UNC\server\share\long\...` passes through
  unchanged.

### 5.5 Relative paths (no prefixing)

- A relative path `foo\bar\baz` (total < MAX_PATH) is returned verbatim
  by `to_extended_path` - no prefix is added.
- A relative path exceeding MAX_PATH when resolved: the test resolves it
  to an absolute path first, then verifies the helper prefixes the
  absolute form.

### 5.6 Forward slashes

- Input `C:/very/long/.../file` is canonicalised to backslashes before
  the `\\?\` prefix is applied (the prefix disables the kernel's
  forward-slash translation).

### 5.7 Embedded `\\?\` mid-path

- Input `C:\foo\\?\bar\baz` is rejected with `io::ErrorKind::InvalidInput`.
- Ensures no path-injection can smuggle a prefix past validation.

## 6. Platform gating and skip conditions

### 6.1 cfg gates

All test modules are wrapped in:

```rust
#![cfg(target_os = "windows")]
```

A `#[cfg(not(target_os = "windows"))]` stub module is not needed -
the tests simply do not compile on other platforms.

### 6.2 Runtime skip conditions

Some tests require environmental preconditions. Use `#[ignore]` with a
doc comment explaining the prerequisite, or a runtime check that prints
a skip reason and returns `Ok(())`:

| Condition | Detection | Tests affected |
|-----------|-----------|----------------|
| Volume is NTFS | `GetVolumeInformationW` returns `"NTFS"` | All (ReFS may differ) |
| Long-path policy disabled | Registry read of `LongPathsEnabled` | W2-W6 (validates `\\?\` is truly required) |
| Symlink privilege available | `CreateSymbolicLinkW` returns success on a probe | Symlink tests |
| ADS support (no ReFS) | `FindFirstStreamW` returns at least `::$DATA` | xattr/ADS tests |

### 6.3 CI environment

The Windows CI leg runs on GitHub Actions `windows-latest` (Windows
Server 2022). The default `LongPathsEnabled` is **0** (disabled) on
Server images, which is exactly the configuration we need - it proves
the `\\?\` prefix is doing the work, not the registry opt-in.

Symlink creation requires the `SeCreateSymbolicLinkPrivilege`, which is
available to `Administrator` on Server 2022 CI images. If unavailable,
symlink tests skip gracefully.

## 7. Test fixture creation strategy

### 7.1 Temporary directory root

All fixtures use `tempfile::tempdir()`. On Windows CI, this resolves
to `C:\Users\RUNNER~1\AppData\Local\Temp\` (approximately 45 chars),
leaving `~215` chars of budget before hitting MAX_PATH for the W1/W2
boundary tests.

### 7.2 Path construction helper

A shared helper builds a path of an exact target length:

```rust
/// Builds a directory path of exactly `target_wide_chars` wide
/// characters (excluding NUL) under `root`, using directory components
/// of `component_len` wide chars each.
///
/// Returns the full path. Does NOT create the directories on disk.
fn build_path_of_length(
    root: &Path,
    target_wide_chars: usize,
    component_len: usize,
) -> PathBuf {
    let root_len = root.as_os_str().encode_wide().count();
    let mut path = root.to_path_buf();
    let mut current_len = root_len;

    while current_len + 1 + component_len < target_wide_chars {
        let name: String = "d".repeat(component_len);
        path = path.join(&name);
        current_len += 1 + component_len; // separator + name
    }

    // Final component sized to hit exact target
    let remaining = target_wide_chars - current_len - 1;
    if remaining > 0 {
        path = path.join("f".repeat(remaining));
    }

    path
}
```

### 7.3 Content generation

Test files contain deterministic content derived from the path itself:

```rust
fn test_content(path: &Path) -> Vec<u8> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let seed = hasher.finish();
    // 4 KB of deterministic bytes
    (0..4096).map(|i| ((seed >> (i % 8)) ^ i as u64) as u8).collect()
}
```

This avoids storing fixture data on disk and makes content-match
assertions self-contained.

### 7.4 Cleanup

`tempfile::TempDir`'s `Drop` impl handles cleanup. On Windows, long
paths in temp directories require `\\?\` for deletion too - the same
`to_extended_path` helper (or a raw `rd /s /q` via `Command`) ensures
cleanup succeeds even if `TempDir::drop` fails with the standard
library's non-prefixed `remove_dir_all`.

A fallback cleanup function is registered via a test helper:

```rust
fn ensure_cleanup(dir: &Path) {
    if std::fs::remove_dir_all(dir).is_err() {
        // Fallback: use cmd.exe which handles long paths natively
        let _ = std::process::Command::new("cmd")
            .args(["/C", "rd", "/s", "/q"])
            .arg(dir)
            .status();
    }
}
```

## 8. Assertion strategy

### 8.1 Content assertions

- Byte-for-byte comparison using `assert_eq!(fs::read(path)?, expected)`.
- For large files (> 64 KB), use streaming comparison with XXH3 to keep
  memory usage bounded and produce useful diff output on failure.

### 8.2 Metadata assertions

- `mtime` within 1-second tolerance (FAT32 granularity; NTFS is 100 ns
  but cross-volume copies may round).
- File size exact match.
- Permissions: read-only flag preserved.
- Symlink target path exact match (not resolved).

### 8.3 Error-path assertions

- Operations on paths exceeding 32767 wide chars produce
  `io::ErrorKind::InvalidInput` from `to_extended_path` (not a raw OS
  error from the kernel).
- Operations with 256-char single components produce NTFS component
  error before the total-length check fires.
- Embedded `\\?\` mid-path produces `io::ErrorKind::InvalidInput` with a
  message containing "embedded extended-length prefix".

### 8.4 Diagnostic message assertions

When `to_extended_path` is not applied (simulated by directly calling
the old `encode_wide + null` path with a > 260 char input), the error
mapper recognises raw OS error 206 and surfaces a user-actionable
message containing "exceeds Win32 MAX_PATH". The test captures the
error message string and asserts the substring is present.

## 9. Test module layout

```
crates/fast_io/tests/
  windows_long_path_io.rs       -- core I/O operations (create, read,
                                   write, rename, delete, stat)
  windows_long_path_symlinks.rs -- symlink creation and resolution
  windows_long_path_roundtrip.rs -- push/pull end-to-end transfers

crates/metadata/tests/
  windows_long_path_acl.rs      -- ACL get/set on long paths
  windows_long_path_xattr.rs    -- ADS read/write on long paths
```

All five files are `#![cfg(target_os = "windows")]` at the module level.

Shared helpers (`build_path_of_length`, `test_content`,
`ensure_cleanup`, `skip_if_not_ntfs`, `skip_if_no_symlink_privilege`)
live in a `tests/windows_helpers.rs` module imported by each test file
via `#[path = "windows_helpers.rs"] mod helpers;`.

## 10. Test matrix

| Test name | Scenario | Operation | Edge case |
|-----------|----------|-----------|-----------|
| `create_file_at_260_boundary` | W1 | create + write | boundary |
| `create_file_at_261_requires_prefix` | W2 | create + write | first overflow |
| `read_write_300_chars` | W3 | create + read + write | common case |
| `deep_nesting_500_chars` | W4 | create_dir_all + write + stat | nested dirs |
| `deep_nesting_1000_chars` | W5 | create_dir_all + write + read + delete | 100+ levels |
| `near_ntfs_ceiling_32000` | W6 | create + write + read | absolute limit |
| `rename_long_path` | W3 | rename (temp commit) | temp-file pattern |
| `delete_file_long_path` | W4 | remove_file | delete emitter |
| `delete_dir_long_path` | W4 | remove_dir | delete emitter |
| `stat_metadata_long_path` | W3 | metadata() | quick-check |
| `symlink_file_long_path` | W3 | symlink_file | requires privilege |
| `symlink_dir_long_path` | W4 | symlink_dir | requires privilege |
| `symlink_long_target` | W3 | symlink where target > 260 | target resolution |
| `acl_get_set_long_path` | W3 | GetNamedSecurityInfoW + Set | ACL round-trip |
| `ads_read_write_long_path` | W3 | stream create + write + read | xattr/ADS |
| `unicode_cjk_long_path` | W4 | create + read | wide-char accounting |
| `unicode_emoji_surrogate` | W3 | create + read | surrogate pair boundary |
| `unicode_combining_chars` | W3 | create + read | no NFC normalisation |
| `trailing_dot_preserved` | W3 | create + read | NTFS quirk |
| `trailing_space_preserved` | W3 | create + read | NTFS quirk |
| `component_255_max` | W4 | create | NTFS component limit |
| `component_256_rejected` | W3 | create (expect error) | NTFS component limit |
| `already_extended_no_double_prefix` | W3 | pass-through | idempotency |
| `unc_path_extended` | W3 | create via `\\?\UNC\` | UNC handling |
| `relative_path_no_prefix` | - | pass-through | relative-path rule |
| `forward_slash_canonicalised` | W3 | create + read | slash translation |
| `embedded_prefix_rejected` | - | expect InvalidInput | security |
| `exceeds_32767_rejected` | beyond W6 | expect InvalidInput | absolute ceiling |
| `error_mapper_os_206` | W3 | simulate non-prefixed call | diagnostic |
| `roundtrip_push_long_dest` | W3+W4+W5 | full transfer push | end-to-end |
| `roundtrip_pull_long_source` | W3+W4+W5 | full transfer pull | end-to-end |
| `roundtrip_bidirectional` | W4 | push + pull back | content parity |

## 11. Integration with CI

The tests run as part of the existing `Windows (stable)` matrix leg in
the CI workflow. No additional workflow file is needed.

Tests that require elevated privileges (symlinks) or specific
environmental state are gated by runtime checks and skip gracefully,
ensuring the overall CI job does not fail due to missing prerequisites.

Expected CI run time for the full Windows long-path suite: < 30 seconds
(path construction and small-file I/O dominate; no large data sets).

## 12. Acceptance criteria

This spec is complete when:

1. All 32 tests in the matrix (section 10) are implemented and passing
   on Windows CI.
2. Tests cover all seven acceptance criteria from WPC-5 section 7.
3. No test relies on `LongPathsEnabled = 1` - all pass with the
   registry key at its default value of 0.
4. Cleanup succeeds without leaving orphaned long-path temp directories.
5. Non-Windows platforms compile cleanly (the test modules are fully
   gated behind `#[cfg(target_os = "windows")]`).

## 13. Cross-references

- WPC-5 audit: `docs/audit/windows-long-path-support.md`.
- WPC-8 reparse classifier: `docs/design/wpc-8-reparse-point-classifier.md`.
- Parent series: #2869 (Windows real-world parity).
- Implementation issue: #2908.
- Existing POSIX long-path tests:
  `crates/engine/src/local_copy/tests/execute_long_paths.rs`,
  `crates/flist/tests/path_max_limits.rs`.
