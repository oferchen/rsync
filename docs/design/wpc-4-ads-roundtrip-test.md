# WPC-4: ADS round-trip regression test

Tracks parent #2869 (Windows real-world parity series). Validates the
WPC-3 implementation (`docs/design/wpc-3-ads-implementation.md`) by
exercising end-to-end alternate data stream preservation and
cross-platform stripping.

## 1. Objective

Prove that oc-rsync correctly round-trips NTFS alternate data streams
via the `-X` / `--xattrs` pipeline, and correctly strips (or
preserves as `user.windows.ads.*` xattrs) when the receiver is a
non-Windows host. Each scenario must assert both data fidelity and
name mapping.

## 2. Scenarios

### 2.1 Core ADS round-trips (Windows-only)

| ID | Scenario | Fixture | Assertions |
|----|----------|---------|------------|
| RT-1 | Single named ADS | File with one `Zone.Identifier` stream (13 bytes) | Stream name, size, and content match after round-trip |
| RT-2 | Multiple named ADS | File with 3 streams: `metadata`, `thumbnail`, `signature` | All 3 streams present; content byte-identical |
| RT-3 | Empty ADS | File with a named stream containing zero bytes | Stream exists; read returns empty `Vec<u8>` |
| RT-4 | Large ADS | File with a 1 MiB stream of pseudo-random data | Content hash matches; no truncation |
| RT-5 | Unicode stream name | Stream named `café` (non-ASCII UTF-8) | Name round-trips without lossy substitution |
| RT-6 | ADS with dots and hyphens | Stream named `com.example.meta-v2` | No confusion with namespace separators |
| RT-7 | ADS on directory | Directory with a named stream | Stream preserved (NTFS supports ADS on dirs) |

### 2.2 Cross-platform matrix

| ID | Direction | `-X` present | Expected behaviour |
|----|-----------|-------------|-------------------|
| XP-1 | Windows -> Windows | yes | Bare ADS names preserved; content identical |
| XP-2 | Windows -> Windows | no | ADS silently dropped; one-shot warning emitted |
| XP-3 | Windows -> Linux | yes | ADS stored as `user.windows.ads.<name>` xattrs; content identical |
| XP-4 | Windows -> Linux | no | ADS dropped; one-shot warning emitted |
| XP-5 | Linux -> Windows | yes | `user.windows.ads.<name>` xattrs reconstituted as bare ADS; prefix stripped |
| XP-6 | Linux -> Windows | no | xattrs dropped (standard behaviour; no ADS-specific logic) |

Cross-platform tests (XP-3..XP-6) cannot run in a single OS process.
The test strategy uses simulated wire payloads (section 4.2) rather
than requiring a multi-OS test matrix at the unit/integration level.

### 2.3 Warning emission

| ID | Condition | Expected |
|----|-----------|----------|
| WN-1 | Windows source, ADS present, no `-X` | Warning emitted exactly once |
| WN-2 | Windows source, ADS present, `-X` given | No warning |
| WN-3 | Windows source, no ADS, no `-X` | No warning |
| WN-4 | Non-Windows source | No warning (probe returns `false`) |
| WN-5 | Multiple ADS-bearing files, no `-X` | Warning fires once (first file), not per-file |

### 2.4 Error and edge cases

| ID | Scenario | Expected |
|----|----------|----------|
| ERR-1 | ADS write on FAT32 volume | `io::Error` propagated; no panic |
| ERR-2 | ADS name containing NUL byte | Rejected by `stream_path_wide` validation |
| ERR-3 | ADS name containing colon | Rejected by `stream_path_wide` validation |
| ERR-4 | ADS read after file deletion | `Ok(None)` or missing-file error |
| ERR-5 | Prefix-strip on name without prefix | Name passed through unchanged |

## 3. Test fixture design

### 3.1 Windows ADS creation (Windows-only tests)

Use the existing `xattr_windows` backend directly:

```rust
use metadata::xattr_windows::{write_attribute, read_attribute, list_attributes};
use tempfile::tempdir;
use std::fs;

fn create_file_with_ads(dir: &Path) -> PathBuf {
    let file = dir.join("source.txt");
    fs::write(&file, b"primary content").expect("write file");
    write_attribute(&file, b"Zone.Identifier", b"[ZoneTransfer]\r\nZoneId=3\r\n", false)
        .expect("write ADS");
    file
}
```

The `ads_supported()` probe (already in `xattr_windows.rs` tests)
guards each test to skip gracefully on non-NTFS CI runners.

### 3.2 Large ADS fixture

Generate deterministic pseudo-random content with a seeded RNG to
avoid storing binary blobs in the repository:

```rust
use std::hash::{DefaultHasher, Hasher};

fn generate_large_payload(size: usize, seed: u64) -> Vec<u8> {
    let mut hasher = DefaultHasher::new();
    let mut buf = Vec::with_capacity(size);
    for i in 0..size {
        hasher.write_u64(seed.wrapping_add(i as u64));
        buf.push((hasher.finish() & 0xFF) as u8);
    }
    buf
}
```

### 3.3 Simulated cross-platform payloads

For XP-3..XP-6, tests simulate the wire-format crossing by:

1. **Windows -> Linux (XP-3):** Call `list_attributes` + `read_attribute`
   on a Windows fixture, then verify the names match the
   `user.windows.ads.<streamname>` convention as seen by a hypothetical
   Linux receiver. Since the Windows backend strips `:` and `:$DATA`
   during enumeration, the bare name is what crosses the wire. The
   protocol layer prepends `user.windows.ads.` on the Linux side.

2. **Linux -> Windows (XP-5):** Construct an `XattrList` with entries
   named `user.windows.ads.Zone.Identifier` (simulating what a Linux
   sender would emit), then call `write_attribute` through the prefix-
   strip path and verify the resulting ADS has the bare name
   `Zone.Identifier`.

This avoids needing a cross-OS network bridge in unit tests while
still exercising the exact code paths.

### 3.4 Warning capture

Warning tests use `tracing-subscriber` with an in-memory layer to
capture emitted warnings. The `AtomicBool` guard is reset between
tests via the existing `EnvGuard`-style test isolation pattern.

## 4. Assertion strategy

### 4.1 Content assertions

- **Byte identity:** `assert_eq!(read_back, original_payload)` for
  exact content round-trip.
- **Hash comparison (large payloads):** For RT-4 (1 MiB), compute
  blake3 or xxh3 hash of original and read-back to produce concise
  failure messages.

### 4.2 Name assertions

- **Exact match:** Stream names must match byte-for-byte after the
  strip/unstrip cycle. No case folding, no normalization.
- **List completeness:** `list_attributes` on the destination must
  return exactly the expected set (no extra streams from NTFS
  inheritance or probe leftovers).

### 4.3 Count assertions

- Number of streams on destination equals number on source (for
  Windows -> Windows with `-X`).
- Zero streams on destination when `-X` absent.

### 4.4 Warning assertions

- Captured warning count equals 1 for WN-1 and WN-5.
- Captured warning count equals 0 for WN-2, WN-3, WN-4.
- Warning text contains the offending file path.

## 5. cfg gates and platform strategy

### 5.1 Windows-only tests

Tests that create or read real NTFS ADS (RT-1..RT-7, XP-1, XP-2,
XP-5, WN-1..WN-3) require:

```rust
#[cfg(windows)]
#[test]
fn ads_single_stream_round_trip() { ... }
```

Each test begins with the `ads_supported()` guard to degrade
gracefully on non-NTFS volumes.

### 5.2 Cross-platform tests

Tests that exercise pure logic without filesystem interaction
(ERR-2, ERR-3, ERR-5, prefix-strip unit tests) compile on all
platforms:

```rust
#[test]
fn strip_ads_prefix_removes_known_prefix() { ... }
```

### 5.3 Non-Windows stub verification

A single test verifies the `has_named_ads` stub returns `Ok(false)`
unconditionally on non-Windows:

```rust
#[cfg(not(windows))]
#[test]
fn has_named_ads_stub_always_false() { ... }
```

### 5.4 CI coverage

The existing `windows-acl-xattr` CI job runs on Windows Server with
NTFS. All `#[cfg(windows)]` tests execute there. Non-Windows tests
run on the Linux and macOS matrix legs.

## 6. Module placement

### 6.1 Unit tests

- `crates/fast_io/src/ads_detect.rs` - inline `#[cfg(test)] mod tests`
  for `has_named_ads` (RT-subset, WN-4, stub test).
- `crates/metadata/src/xattr_windows.rs` - extend existing inline
  tests with RT-1..RT-7 and prefix-strip assertions.

### 6.2 Integration tests

Create a new integration test file:

```
crates/metadata/tests/ads_round_trip.rs
```

This file exercises the full xattr cross-platform layer (not just the
Windows backend directly) to verify the end-to-end path through
`apply_xattrs` and `collect_xattrs`. It imports from the public
`metadata` API and uses `tempfile::tempdir` for fixtures.

### 6.3 Warning integration test

```
crates/engine/tests/ads_warning.rs
```

Exercises the one-shot warning logic at the engine level. On Windows,
creates an ADS-bearing file and runs the flist walker without `-X`;
asserts the warning fires. On non-Windows, asserts no warning fires
regardless of file content.

## 7. Integration with existing infrastructure

| Component | Usage |
|-----------|-------|
| `tempfile::tempdir()` | All test fixtures use ephemeral directories |
| `ads_supported()` guard | Graceful skip on non-NTFS CI runners |
| `protocol::xattr::{XattrEntry, XattrList}` | Construct simulated wire payloads |
| `tracing-subscriber` test layer | Warning capture for WN-* tests |
| `EnvGuard` | Reset global `AtomicBool` state between tests |
| `std::fs::write` / `std::fs::read` | Primary content creation (file body) |
| Seeded RNG | Deterministic large payloads without binary fixtures |

## 8. Test execution order and isolation

Each test is fully independent - no shared mutable state beyond the
`AtomicBool` warning guard (which is reset per-test). Tests may run
in parallel via nextest without interference because each creates its
own `TempDir`.

The `AtomicBool` guard for the one-shot warning requires explicit
reset in warning tests. The test helper resets it before each WN-*
test case:

```rust
fn reset_ads_warning_guard() {
    ADS_WARNING_EMITTED.store(false, Ordering::SeqCst);
}
```

This reset function is `#[cfg(test)]` gated and exposed via a
`pub(crate)` test-support path.

## 9. Acceptance criteria

1. All RT-* tests pass on the Windows CI runner with NTFS.
2. All XP-* tests pass - simulated cross-platform payloads verify
   prefix-strip and prefix-add logic.
3. All WN-* tests pass - warning fires exactly once when expected,
   never when not.
4. All ERR-* tests pass on all platforms.
5. No test relies on a specific drive letter, user SID, or inherited
   ACL - fixtures are self-contained.
6. Tests degrade gracefully (skip with message) on non-NTFS volumes.
7. No new Cargo features required - tests use existing `--all-features`
   flag.

## 10. Cross-references

- WPC-1 audit: `docs/audit/windows-ads-handling.md` (#2903).
- WPC-2 strategy: `docs/design/windows-ads-strategy.md` (#2904).
- WPC-3 implementation: `docs/design/wpc-3-ads-implementation.md` (#2905).
- Parent: #2869 (Windows real-world parity series).
- CI: `docs/design/windows-acl-xattr-ci-matrix.md`.
- Existing test patterns: `crates/metadata/src/xattr_windows.rs` (inline tests),
  `crates/metadata/src/acl_windows/tests/xattr.rs` (SDDL round-trip).
