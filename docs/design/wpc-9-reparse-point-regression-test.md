# Reparse-point regression test spec (WPC-9)

Tracks parent #2869 (Windows real-world parity series). Follows WPC-8
(#2910, `docs/design/wpc-8-reparse-point-classifier.md`) which ships
the classifier module. Tests verify that `classify_reparse`,
`parse_reparse_buffer`, and `read_reparse_data` correctly identify
every reparse-point kind and that the transfer pipeline handles each
kind with the correct behaviour.

## 1. Goals

- Achieve 100% branch coverage on `classify_reparse` and
  `parse_reparse_buffer` across all platforms.
- Validate `read_reparse_data` round-trips on Windows CI against
  real filesystem reparse points.
- Verify transfer-behaviour decisions (skip, warn, treat-as-symlink,
  treat-as-file) for every `ReparseKind` variant.
- Provide deterministic cross-platform unit tests via synthetic
  reparse buffers - no Windows APIs required.
- Gate integration tests that require real reparse creation behind
  `#[cfg(windows)]` and privilege-dependent tests behind `#[ignore]`.

## 2. Test module layout

```
crates/metadata/src/windows/reparse.rs
    #[cfg(test)]
    mod tests {
        // Section 3: classify_reparse unit tests (all platforms)
        // Section 4: parse_reparse_buffer unit tests (all platforms)
    }

crates/metadata/tests/
    reparse_integration.rs          // #[cfg(windows)] integration tests
                                    // Section 5: real filesystem round-trips

crates/transfer/src/generator/file_list/
    #[cfg(test)]
    mod reparse_transfer_tests {
        // Section 6: transfer-behaviour decision tests
    }
```

The unit tests in `reparse.rs` compile on all platforms because
`classify_reparse` and `parse_reparse_buffer` are pure functions
over byte slices and integer constants. Integration tests compile
only on Windows.

## 3. Unit tests - classify_reparse (all platforms)

These tests exercise the tag-to-kind dispatch table from WPC-8
section 3.2. Each arm of the `match` has a dedicated test.

### 3.1 Named tags

| Test name | Input tag | Expected output |
|---|---|---|
| `classify_symlink` | `0xA000_000C` | `Symlink` |
| `classify_mount_point_default` | `0xA000_0003` | `Junction` |
| `classify_appexeclink` | `0x8000_001B` | `AppExecLink` |
| `classify_wci` | `0x8000_0018` | `Wci` |
| `classify_global_reparse` | `0xA000_0019` | `GlobalReparse` |
| `classify_lx_symlink` | `0xA000_001D` | `LxSymlink` |
| `classify_lx_fifo` | `0x8000_0024` | `LxFifo` |
| `classify_lx_chr` | `0x8000_0025` | `LxChr` |
| `classify_lx_blk` | `0x8000_0026` | `LxBlk` |
| `classify_af_unix` | `0x8000_0023` | `AfUnix` |
| `classify_projfs` | `0x9000_001C` | `ProjFs` |
| `classify_wof` | `0x8000_0017` | `Wof` |
| `classify_hsm_v1` | `0xC000_0004` | `Hsm` |
| `classify_hsm_v2` | `0x8000_0006` | `Hsm` |

### 3.2 Cloud range

| Test name | Input tag | Expected output |
|---|---|---|
| `classify_cloud_min` | `0x9000_001A` | `Cloud(0x9000_001A)` |
| `classify_cloud_max` | `0x9000_031A` | `Cloud(0x9000_031A)` |
| `classify_cloud_mid` | `0x9000_0200` | `Cloud(0x9000_0200)` |
| `classify_below_cloud_range` | `0x9000_0019` | `Other(0x9000_0019)` |
| `classify_above_cloud_range` | `0x9000_031B` | `Other(0x9000_031B)` |

### 3.3 Unknown / other tags

| Test name | Input tag | Expected output |
|---|---|---|
| `classify_unknown_tag` | `0xDEAD_BEEF` | `Other(0xDEAD_BEEF)` |
| `classify_zero_tag` | `0x0000_0000` | `Other(0x0000_0000)` |
| `classify_all_ones` | `0xFFFF_FFFF` | `Other(0xFFFF_FFFF)` |
| `classify_near_miss_appexec` | `0x8000_001C` | `Other(0x8000_001C)` |

### 3.4 has_reparse_point predicate

| Test name | Input attrs | Expected |
|---|---|---|
| `has_reparse_set` | `0x0400` | `true` |
| `has_reparse_combined` | `0x0420` (reparse + directory) | `true` |
| `has_reparse_not_set` | `0x0020` | `false` |
| `has_reparse_zero` | `0x0000` | `false` |

## 4. Unit tests - parse_reparse_buffer (all platforms)

These tests construct synthetic reparse buffers as `&[u8]` and feed
them to `parse_reparse_buffer`. The function is factored out of
`read_reparse_data` per WPC-8 section 8.2 specifically for
cross-platform testability.

### 4.1 Helper - buffer construction

A test helper module provides builders for synthetic reparse buffers:

```rust
/// Builds a minimal reparse buffer with the given tag and payload.
fn build_reparse_header(tag: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&tag.to_le_bytes());          // offset 0: tag
    buf.extend_from_slice(&(payload.len() as u16).to_le_bytes()); // offset 4: data length
    buf.extend_from_slice(&0u16.to_le_bytes());         // offset 6: reserved
    buf.extend_from_slice(payload);                      // offset 8: payload
    buf
}

/// Builds a symlink reparse buffer (tag 0xA000_000C) with the given
/// UTF-16LE substitute name and flags.
fn build_symlink_buffer(substitute: &str, flags: u32) -> Vec<u8>;

/// Builds a mount-point / junction reparse buffer (tag 0xA000_0003)
/// with the given UTF-16LE substitute name.
fn build_mount_point_buffer(substitute: &str) -> Vec<u8>;

/// Builds an LX_SYMLINK reparse buffer (tag 0xA000_001D) with
/// 4-byte flags followed by the UTF-8 POSIX target.
fn build_lx_symlink_buffer(target: &str) -> Vec<u8>;
```

### 4.2 Symlink buffer tests

| Test name | Buffer | Assertions |
|---|---|---|
| `parse_symlink_absolute` | `build_symlink_buffer(r"\??\C:\target\file.txt", 0)` | `kind == Symlink`, `target == Some(r"\??\C:\target\file.txt")` |
| `parse_symlink_relative` | `build_symlink_buffer(r"..\other\file.txt", 1)` | `kind == Symlink`, `target == Some(r"..\other\file.txt")` |
| `parse_symlink_unc` | `build_symlink_buffer(r"\??\UNC\server\share\path", 0)` | `kind == Symlink`, `target` contains the UNC path |
| `parse_symlink_empty_target` | `build_symlink_buffer("", 0)` | `kind == Symlink`, `target == Some("")` |

### 4.3 Junction buffer tests

| Test name | Buffer | Assertions |
|---|---|---|
| `parse_junction_local` | `build_mount_point_buffer(r"\??\C:\Users\Public")` | `kind == Junction`, `target == Some(r"\??\C:\Users\Public")` |
| `parse_junction_unc` | `build_mount_point_buffer(r"\??\UNC\server\share")` | `kind == Junction`, `target` contains the UNC path |
| `parse_junction_nested_path` | `build_mount_point_buffer(r"\??\D:\deep\nested\path\target")` | `kind == Junction`, correct target |

### 4.4 Volume mount-point buffer tests

| Test name | Buffer | Assertions |
|---|---|---|
| `parse_mount_point_volume_guid` | `build_mount_point_buffer(r"\??\Volume{12345678-abcd-1234-abcd-123456789abc}\")` | `kind == MountPoint`, `target` contains the volume GUID path |
| `parse_mount_point_volume_guid_subdir` | `build_mount_point_buffer(r"\??\Volume{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}\subdir")` | `kind == MountPoint`, correct target |

### 4.5 WSL LxSymlink buffer tests

| Test name | Buffer | Assertions |
|---|---|---|
| `parse_lx_symlink_absolute` | `build_lx_symlink_buffer("/home/user/link")` | `kind == LxSymlink`, `target == Some("/home/user/link")` |
| `parse_lx_symlink_relative` | `build_lx_symlink_buffer("../relative/target")` | `kind == LxSymlink`, correct target |
| `parse_lx_symlink_with_null` | payload: flags + `b"/tmp/target\0"` | `kind == LxSymlink`, target strips trailing null |
| `parse_lx_symlink_empty` | `build_lx_symlink_buffer("")` | `kind == LxSymlink`, `target == Some("")` |

### 4.6 Opaque-kind buffer tests

These tags produce `target == None` because their payloads are
provider-private and not user-meaningful.

| Test name | Tag | Assertions |
|---|---|---|
| `parse_cloud_placeholder` | `0x9000_001A` | `kind == Cloud(0x9000_001A)`, `target == None` |
| `parse_cloud_onedrive` | `0x9000_0014` (OneDrive sub-variant within range) | `kind == Cloud(...)`, `target == None` |
| `parse_appexeclink` | `0x8000_001B` | `kind == AppExecLink`, `target == None` |
| `parse_wci` | `0x8000_0018` | `kind == Wci`, `target == None` |
| `parse_projfs` | `0x9000_001C` | `kind == ProjFs`, `target == None` |
| `parse_wof` | `0x8000_0017` | `kind == Wof`, `target == None` |
| `parse_hsm_v1` | `0xC000_0004` | `kind == Hsm`, `target == None` |
| `parse_unknown` | `0xDEAD_BEEF` | `kind == Other(0xDEAD_BEEF)`, `target == None` |

### 4.7 Error-path tests

| Test name | Buffer | Expected error |
|---|---|---|
| `parse_empty_buffer` | `&[]` | `InvalidData` |
| `parse_truncated_header` | 4 bytes only | `InvalidData` |
| `parse_header_only` | 8-byte header, zero-length payload for symlink tag | `InvalidData` (payload too short for sub-name offsets) |
| `parse_symlink_subname_overflow` | header claims 200-byte sub-name, buffer has 20 bytes | `InvalidData` |
| `parse_mount_point_subname_overflow` | same pattern for mount-point tag | `InvalidData` |
| `parse_lx_symlink_no_flags` | tag `0xA000_001D`, payload shorter than 4 bytes | `InvalidData` |
| `parse_odd_length_utf16` | symlink buffer with odd-byte-count sub-name length | `InvalidData` |

### 4.8 Raw buffer preservation

| Test name | Scenario | Assertions |
|---|---|---|
| `raw_buffer_preserved_symlink` | Parse a symlink buffer | `result.raw == original_buffer` |
| `raw_buffer_preserved_cloud` | Parse a cloud buffer | `result.raw == original_buffer` |
| `raw_buffer_preserved_unknown` | Parse an unknown-tag buffer | `result.raw == original_buffer` |

## 5. Integration tests - read_reparse_data (Windows only)

These tests create real reparse points via Win32 APIs or shell
commands and verify the full I/O round-trip through
`read_reparse_data`. All tests in this module are gated with
`#[cfg(windows)]`.

### 5.1 Fixture creation

Each test creates fixtures in a `tempfile::TempDir` using the
following approaches:

| Reparse kind | Creation method |
|---|---|
| File symlink | `std::os::windows::fs::symlink_file(target, link)` |
| Directory symlink | `std::os::windows::fs::symlink_dir(target, link)` |
| Junction | `Command::new("cmd").args(["/C", "mklink", "/J", link, target])` |
| Mount point | `mountvol` (requires admin) - `#[ignore]` |
| WSL symlink | `Command::new("wsl").args(["ln", "-s", target, link])` - `#[ignore]` |
| Cloud placeholder | Requires OneDrive provider - `#[ignore]` |
| AppExecLink | Exists in `%LOCALAPPDATA%\Microsoft\WindowsApps` - `#[ignore]` |

Fixtures requiring elevated privileges or third-party providers
carry `#[ignore]` and include a docstring with a manual reproduction
recipe.

### 5.2 File symlink round-trip

```rust
#[test]
#[cfg(windows)]
fn read_reparse_file_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    std::fs::write(&target, b"content").unwrap();
    let link = dir.path().join("link.txt");
    std::os::windows::fs::symlink_file(&target, &link).unwrap();

    let data = read_reparse_data(&link).unwrap();
    assert_eq!(data.kind, ReparseKind::Symlink);
    assert!(data.target.is_some());
    // Target path contains the substitute name
    let target_str = data.target.unwrap().to_string_lossy().to_string();
    assert!(target_str.contains("target.txt"));
    assert!(!data.raw.is_empty());
}
```

### 5.3 Directory symlink round-trip

```rust
#[test]
#[cfg(windows)]
fn read_reparse_dir_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target_dir");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("link_dir");
    std::os::windows::fs::symlink_dir(&target, &link).unwrap();

    let data = read_reparse_data(&link).unwrap();
    assert_eq!(data.kind, ReparseKind::Symlink);
    assert!(data.target.is_some());
}
```

### 5.4 Junction round-trip

```rust
#[test]
#[cfg(windows)]
fn read_reparse_junction() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target_dir");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("junction");

    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());

    let data = read_reparse_data(&link).unwrap();
    assert_eq!(data.kind, ReparseKind::Junction);
    assert!(data.target.is_some());
    let target_str = data.target.unwrap().to_string_lossy().to_string();
    // Junctions use \??\ prefix, not \??\Volume{GUID}\
    assert!(target_str.starts_with(r"\??\"));
    assert!(!target_str.contains("Volume{"));
}
```

### 5.5 Non-reparse file (negative test)

```rust
#[test]
#[cfg(windows)]
fn read_reparse_regular_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("regular.txt");
    std::fs::write(&file, b"not a reparse point").unwrap();

    let result = read_reparse_data(&file);
    assert!(result.is_err());
}
```

### 5.6 Dangling symlink

```rust
#[test]
#[cfg(windows)]
fn read_reparse_dangling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("dangling");
    // Create symlink to a non-existent target
    std::os::windows::fs::symlink_file(r"C:\nonexistent\target.txt", &link)
        .unwrap();

    // read_reparse_data should still succeed - it opens with
    // FILE_FLAG_OPEN_REPARSE_POINT, not following the link
    let data = read_reparse_data(&link).unwrap();
    assert_eq!(data.kind, ReparseKind::Symlink);
    assert!(data.target.is_some());
}
```

### 5.7 Dangling junction

```rust
#[test]
#[cfg(windows)]
fn read_reparse_dangling_junction() {
    let dir = tempfile::tempdir().unwrap();
    let junction = dir.path().join("dangling_junc");
    let fake_target = dir.path().join("no_such_dir");

    // mklink /J succeeds even when target doesn't exist
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&fake_target)
        .status()
        .unwrap();
    assert!(status.success());

    let data = read_reparse_data(&junction).unwrap();
    assert_eq!(data.kind, ReparseKind::Junction);
}
```

### 5.8 Volume mount point (ignored - requires admin)

```rust
#[test]
#[cfg(windows)]
#[ignore] // Requires admin: mountvol <dir> <volume-guid>
/// Manual recipe:
/// 1. Run `mountvol` to list available volume GUIDs
/// 2. `mkdir C:\MountTest`
/// 3. `mountvol C:\MountTest \\?\Volume{GUID}\`
/// 4. Run this test with `--include-ignored`
/// 5. Cleanup: `mountvol C:\MountTest /D && rmdir C:\MountTest`
fn read_reparse_volume_mount_point() {
    // Test requires a pre-existing mount point at a known path
    // or admin privileges to create one via mountvol.
}
```

### 5.9 WSL symlink (ignored - requires WSL)

```rust
#[test]
#[cfg(windows)]
#[ignore] // Requires WSL installed and accessible
/// Manual recipe:
/// 1. Ensure WSL is installed: `wsl --status`
/// 2. Create symlink: `wsl ln -s /tmp/target /mnt/c/Users/.../wsl_link`
/// 3. Run this test with `--include-ignored`
fn read_reparse_wsl_symlink() {
    // Verify LxSymlink kind and POSIX target extraction
}
```

### 5.10 AppExecLink (ignored - requires Store app)

```rust
#[test]
#[cfg(windows)]
#[ignore] // Requires a Windows Store app alias to exist
/// Manual recipe:
/// 1. Ensure a Store app is installed (e.g., Windows Terminal)
/// 2. Locate alias in %LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe
/// 3. Pass that path to read_reparse_data
/// 4. Assert kind == AppExecLink
fn read_reparse_appexeclink() {
    // The WindowsApps directory contains AppExecLink reparse points
    // for every Store app that registers a command alias.
}
```

## 6. Transfer-behaviour tests

These tests verify that the file-list build and transfer pipeline
make correct decisions for each `ReparseKind`. They do not require
real reparse points - they mock the classifier output and assert
the routing decision.

### 6.1 Test approach

The transfer-behaviour tests operate at the level of the
file-entry construction logic. Given a `ReparseData` value, they
assert which branch the entry-build code takes:

- **Symlink branch**: entry is tagged as a symlink with the
  provided target path.
- **Skip with warning**: entry is not added to the file list; a
  warning is emitted.
- **Regular-file branch**: entry is treated as a regular file
  (triggers hydration for cloud placeholders).
- **Special-file branch**: entry is routed to device/FIFO handling.

### 6.2 Test matrix

| `ReparseKind` | Expected route | Rationale |
|---|---|---|
| `Symlink` | Symlink branch, target from `ReparseData.target` | Genuine NTFS symlink; matches upstream rsync behaviour |
| `Junction` | Symlink branch, target from `ReparseData.target` | Junction-as-symlink per WPC-8 section 6.1 and R1 |
| `MountPoint` | Skip with warning | Volume mount points cannot be safely reconstructed (R1) |
| `Cloud(tag)` | Regular-file branch; INFO log on first occurrence | Hydration is correct for backup; one-shot log per R2 |
| `AppExecLink` | Skip with warning | Provider-private blob; non-functional on destination (R4) |
| `Wci` | Skip with warning | Container-isolation metadata; non-portable (R4) |
| `GlobalReparse` | Regular-file branch | Bind-mount; underlying content is correct to transfer |
| `LxSymlink` | Symlink branch, POSIX target from buffer | Decoded WSL symlink; target is UTF-8 POSIX path (R3) |
| `LxFifo` | Special-file branch (FIFO) | WSL FIFO; skip on non-Unix destination |
| `LxChr` | Special-file branch (char device) | WSL char device; skip on non-Unix destination |
| `LxBlk` | Special-file branch (block device) | WSL block device; skip on non-Unix destination |
| `AfUnix` | Special-file branch (socket) | WSL AF_UNIX; skip on non-Unix destination |
| `ProjFs` | Regular-file branch | Projected FS; transparent hydration is correct |
| `Wof` | Regular-file branch | Windows Overlay; compact OS transparently serves content |
| `Hsm` | Regular-file branch | HSM auto-hydrates on read; content arrives correctly |
| `Other(tag)` | Regular-file branch | Unknown; conservative default matches current behaviour |

### 6.3 Warning-emission tests

For kinds that trigger skip-with-warning (`MountPoint`,
`AppExecLink`, `Wci`), verify:

- The entry is absent from the resulting file list.
- A per-path warning message is emitted containing the path and the
  reparse tag hex value.
- The warning is emitted at most once per path (not per re-scan).

### 6.4 Cloud INFO log test

For `Cloud(tag)` entries, verify:

- The first cloud placeholder encountered emits an INFO-level log
  naming the path and the tag.
- Subsequent cloud placeholders in the same transfer do not emit
  additional INFO logs (one-shot behaviour).
- The entry is added to the file list as a regular file with the
  placeholder's logical size.

### 6.5 WSL special-file cross-platform test

For `LxFifo`, `LxChr`, `LxBlk`, `AfUnix`:

- On a Unix-targeted transfer: entry is routed to the special-file
  branch with the correct file type (FIFO, CHR, BLK, SOCK).
- On a Windows-targeted transfer: entry is skipped with a warning
  explaining that POSIX special files cannot be represented on the
  destination.

## 7. Edge-case tests

### 7.1 Dangling junctions

- Create a junction pointing to a non-existent directory.
- Verify that `read_reparse_data` succeeds (opens the reparse
  container, not the target).
- Verify that the file-list build routes it to the symlink branch
  with the non-existent target path.
- Verify that the transfer pipeline does not error - the symlink
  target is stored verbatim, same as a dangling POSIX symlink.

### 7.2 Cross-volume mount points

- Construct a synthetic mount-point buffer with a
  `\??\Volume{GUID}\` substitute-name.
- Verify `parse_reparse_buffer` returns `MountPoint`.
- Verify the transfer-behaviour test skips the entry with a warning.

### 7.3 Nested junctions

- Create a directory tree: `A/B/C` where `B` is a junction pointing
  to `D/E`.
- Verify that without `--copy-links`, the walker treats `B` as a
  symlink and does not descend into the junction target.
- Verify that the file list contains `B` as a symlink entry, not a
  directory with recursive contents.

### 7.4 Junction pointing to ancestor (loop detection)

- Create a directory `parent/child` where `child` is a junction
  pointing to `parent`.
- Verify that without `--copy-links`, the junction is recorded as a
  symlink and no recursion occurs.
- Verify that with `--copy-links`, the walk either detects the loop
  (via inode/path tracking) or produces a bounded error rather than
  infinite recursion.

### 7.5 Symlink with maximum-length target

- Construct a symlink buffer with a 32,766-character target path
  (MAX_PATH limit in the substitute-name field).
- Verify `parse_reparse_buffer` succeeds and returns the full
  target.

### 7.6 Junction with Unicode path

- Construct a junction buffer with a substitute-name containing
  non-ASCII UTF-16 characters (e.g., CJK ideographs, emoji).
- Verify `parse_reparse_buffer` correctly decodes the UTF-16LE
  substitute-name to a Rust `PathBuf`.

### 7.7 Multiple reparse points in a single directory listing

- Create a `tempdir` containing a mix: 1 file symlink, 1 dir
  symlink, 1 junction, 1 regular file.
- Run the file-list walker over the directory.
- Assert that each entry is classified correctly and the file list
  contains the expected entry types.

## 8. cfg gates and platform stubs

### 8.1 Compilation matrix

| Module | Linux | macOS | Windows |
|---|---|---|---|
| `classify_reparse` unit tests | Compiles and runs | Compiles and runs | Compiles and runs |
| `parse_reparse_buffer` unit tests | Compiles and runs | Compiles and runs | Compiles and runs |
| `read_reparse_data` integration tests | Does not compile | Does not compile | Compiles and runs |
| Transfer-behaviour tests | Compiles and runs | Compiles and runs | Compiles and runs |

### 8.2 Gate patterns

```rust
// Unit tests: no gates needed (pure functions)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_symlink() { /* ... */ }
}

// Integration tests: Windows-only
#[cfg(all(test, windows))]
mod integration_tests {
    use super::*;

    #[test]
    fn read_reparse_file_symlink() { /* ... */ }

    #[test]
    #[ignore] // Requires admin privileges
    fn read_reparse_volume_mount_point() { /* ... */ }
}
```

### 8.3 Transfer-behaviour test stubs

The transfer-behaviour tests (section 6) need to inject a
`ReparseData` without actually calling `read_reparse_data`. The test
constructs a `ReparseData` struct directly:

```rust
let mock_data = ReparseData {
    kind: ReparseKind::Junction,
    target: Some(PathBuf::from(r"\??\C:\Users\Public")),
    raw: build_mount_point_buffer(r"\??\C:\Users\Public"),
};
```

This avoids any platform dependency - the `ReparseData` struct is
just a Rust data structure with no Windows API ties.

## 9. Test fixture creation reference

For integration tests on Windows CI, the following commands create
each fixture type:

| Fixture | Creation command | Cleanup |
|---|---|---|
| File symlink | `mklink link.txt target.txt` | `del link.txt` |
| Directory symlink | `mklink /D link_dir target_dir` | `rmdir link_dir` |
| Junction | `mklink /J junction target_dir` | `rmdir junction` |
| Mount point | `mountvol <dir> \\?\Volume{GUID}\` (admin) | `mountvol <dir> /D` |
| WSL symlink | `wsl ln -s /posix/target /mnt/c/.../link` | `wsl rm /mnt/c/.../link` |

In Rust test code, prefer `std::os::windows::fs::symlink_file` and
`symlink_dir` over shell commands where possible. Use
`std::process::Command` for junction creation (`mklink /J` has no
std equivalent). All fixtures live in `tempfile::TempDir` instances
that clean up automatically on test completion.

### 9.1 Privilege requirements

| Fixture type | Requires admin | Requires Developer Mode | Requires WSL |
|---|---|---|---|
| File symlink | No (Win10 1703+ with Developer Mode) | Yes | No |
| Directory symlink | No (Win10 1703+ with Developer Mode) | Yes | No |
| Junction | No | No | No |
| Mount point | Yes | No | No |
| WSL symlink | No | No | Yes |
| AppExecLink | No (read-only; created by Store) | No | No |

Windows CI runners in GitHub Actions have Developer Mode enabled by
default, so symlink and junction tests run without `#[ignore]`.
Mount-point and WSL tests require `#[ignore]`.

## 10. Coverage targets

| Component | Target | Measurement |
|---|---|---|
| `classify_reparse` | 100% branch | All match arms exercised |
| `parse_reparse_buffer` | 100% branch | All tag paths + all error paths |
| `has_reparse_point` | 100% line | Trivial - 2 tests cover it |
| `read_reparse_data` | Best-effort on Windows CI | I/O-dependent; core paths covered by integration tests |
| Transfer-behaviour routing | 100% variant | Every `ReparseKind` variant has a dedicated test |

## 11. Acceptance criteria

- [ ] Unit tests for `classify_reparse` pass on Linux, macOS, and
      Windows CI.
- [ ] Unit tests for `parse_reparse_buffer` pass on all platforms
      with synthetic buffers covering every documented tag.
- [ ] Error-path tests verify `InvalidData` for truncated, overflow,
      and malformed buffers.
- [ ] Integration tests for `read_reparse_data` pass on Windows CI
      for file symlinks, directory symlinks, junctions, dangling
      symlinks, dangling junctions, and non-reparse files.
- [ ] `#[ignore]`-gated tests exist for mount points, WSL symlinks,
      and AppExecLinks with documented manual reproduction recipes.
- [ ] Transfer-behaviour tests verify the routing decision for every
      `ReparseKind` variant.
- [ ] Edge-case tests cover dangling junctions, cross-volume mount
      points, nested junctions, ancestor-pointing junctions,
      max-length targets, and Unicode paths.
- [ ] No `#[cfg(windows)]` gate on unit tests that exercise pure
      functions.
- [ ] All tests use `tempfile::TempDir` for filesystem fixtures.
- [ ] `cargo clippy --workspace --all-targets --all-features` passes
      on all platforms.
- [ ] `cargo fmt --all -- --check` passes.

## 12. Cross-references

Internal:

- `docs/design/wpc-8-reparse-point-classifier.md` (WPC-8, #2910) -
  the implementation this test suite validates.
- `docs/audit/windows-reparse-point-classification.md` (WPC-7,
  #2909) - audit findings informing test scenarios.
- `docs/audit/windows-acl-xattr-ci-matrix.md` - test-fixture
  pattern template (deterministic per-feature fixtures gated on
  Win32 availability).
- `docs/user/windows-support-matrix.md` (WPC-13, #4920) -
  user-facing matrix that will move from Partial to Supported once
  WPC-8 and WPC-9 land.

Tracking:

- Parent: **#2869** (Windows real-world parity series).
- Predecessor: **#2910** (WPC-8, classifier implementation).
- This document: **#2911** (WPC-9).
