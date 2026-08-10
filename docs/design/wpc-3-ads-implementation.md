# WPC-3: ADS implementation spec

Tracks parent #2869 (Windows real-world parity series). Implements the
WPC-2 decision (`docs/design/windows-ads-strategy.md`). Feeds WPC-4
(#2906, regression test).

## 1. WPC-2 decision summary

WPC-2 selected option (a) - xattr passthrough. NTFS alternate data
streams surface through the existing `-X` / `--xattrs` pipeline as
standard xattr entries. No new CLI flag, no new wire frame, no new
capability bit. The implementation adds two deliverables on top of the
already-shipping code in `crates/metadata/src/xattr_windows.rs`:

1. A one-shot warning when `-X` is absent and the source carries ADS.
2. A man-page entry documenting the ADS-as-xattr mapping.

Additionally, the WPC-2 spec requires verification that the
`user.windows.ads.<streamname>` prefix-stripping on the write path is
correct for cross-platform round-trips (Linux/macOS sender to Windows
receiver). WPC-1 confirmed the read path already produces bare stream
names; the write path needs a prefix-strip guard.

## 2. Module placement

The WPC-2 spec names `fast_io::windows::ads` as the target module.
After reviewing the codebase, the correct placement is:

- **ADS detection probe** - `crates/fast_io/src/ads_detect.rs`. A
  thin, Windows-only module that probes whether a path has named ADS.
  Non-Windows targets get a stub returning `false`. This belongs in
  `fast_io` because it wraps `FindFirstStreamW` (a platform I/O
  primitive) behind a safe API, matching the crate's role as the
  unsafe-boundary isolator.
- **Prefix-strip guard** - `crates/metadata/src/xattr_windows.rs`.
  The existing module already owns the full read/write/list/remove
  surface. The prefix-stripping logic for cross-platform round-trips
  belongs here, not in `fast_io`, because it is a naming-convention
  concern local to the xattr wire mapping.
- **Warning emission** - `crates/engine/src/` (generator or flist
  walker). The one-shot warning fires at the sender side during file
  enumeration, which is engine-level orchestration code.

The flat `ads_detect.rs` file in `fast_io/src/` follows the existing
pattern used by `refs_detect.rs`, `copy_file_ex.rs`, and
`sendfile.rs` - single-file modules for platform-specific I/O probes.
A nested `windows/ads/` directory is not warranted because there is
only one function.

## 3. Public API surface

### 3.1 `fast_io::ads_detect`

```rust
/// Returns `true` when `path` has at least one named NTFS alternate
/// data stream (excluding the unnamed primary stream `::$DATA`).
///
/// Used by the sender-side flist walker to decide whether to emit
/// the one-shot ADS warning when `-X` is not present.
///
/// On non-Windows targets, always returns `Ok(false)`.
pub fn has_named_ads(path: &Path) -> io::Result<bool>;
```

Windows implementation calls `FindFirstStreamW`, iterates with
`FindNextStreamW`, and returns `true` on the first non-primary
`$DATA` stream. It short-circuits - no allocation, no full
enumeration. Errors from non-NTFS volumes (`ERROR_HANDLE_EOF`,
`ERROR_NO_MORE_FILES`) map to `Ok(false)`.

Non-Windows stub:

```rust
#[cfg(not(windows))]
pub fn has_named_ads(_path: &Path) -> io::Result<bool> {
    Ok(false)
}
```

### 3.2 `metadata::xattr_windows` - prefix-strip guard

Add a public constant and a helper in the existing
`xattr_windows.rs`:

```rust
/// Namespace prefix prepended to ADS stream names when they cross
/// the wire to non-Windows receivers.
pub const ADS_XATTR_PREFIX: &str = "user.windows.ads.";

/// Strips the `user.windows.ads.` prefix from `name` before passing
/// it to `stream_path_wide`, so a Linux-sourced xattr entry
/// reconstitutes the correct bare ADS name on a Windows receiver.
///
/// Returns the original name unchanged if no prefix is present
/// (Windows-to-Windows transfer where names are already bare).
pub fn strip_ads_prefix(name: &[u8]) -> &[u8];
```

The `write_attribute` function calls `strip_ads_prefix(name)` before
delegating to `stream_path_wide`. This is the only code change in the
existing module.

### 3.3 Warning type

No new warning type is needed. The warning uses the existing
`tracing::warn!` path, emitted once per transfer via an
`AtomicBool` guard.

```rust
/// One-shot guard for the ADS-without-xattrs warning.
static ADS_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
```

Warning text (exact, per WPC-2 section 4.3):
```
warning: windows alternate data streams on <path> will not be preserved without --xattrs (-X)
```

## 4. Implementation approach

### 4.1 Windows APIs used

All APIs are already linked by `crates/metadata/src/xattr_windows.rs`
via the `windows` crate. The `fast_io` module reuses the same Win32
surface:

| API | Purpose |
|-----|---------|
| `FindFirstStreamW` | Begin stream enumeration |
| `FindNextStreamW` | Continue enumeration |
| `FindClose` | Release search handle |

The `fast_io` crate already depends on `windows-sys` for Windows
targets (see `Cargo.toml` line 173). `has_named_ads` uses
`windows-sys` directly (not the higher-level `windows` crate) to stay
consistent with fast_io's existing Windows code (`copy_file_ex.rs`,
`iocp/`).

### 4.2 Prefix-strip logic

```rust
pub fn strip_ads_prefix(name: &[u8]) -> &[u8] {
    let prefix = ADS_XATTR_PREFIX.as_bytes();
    name.strip_prefix(prefix).unwrap_or(name)
}
```

Called at the top of `write_attribute` before the name reaches
`stream_path_wide`. Zero allocation, zero copy.

### 4.3 Warning emission point

The warning fires in the sender-side file-list walker when:

1. `cfg!(windows)` is true (compile-time gate).
2. `preserve_xattrs` is false in the transfer config.
3. `has_named_ads(&path)` returns `Ok(true)` for any enumerated file.
4. `ADS_WARNING_EMITTED.compare_exchange(false, true, ...)` succeeds
   (at-most-once guard).

The probe cost is one `FindFirstStreamW` + at most one
`FindNextStreamW` per file. On files with no named streams the
enumeration completes in a single syscall returning `ERROR_HANDLE_EOF`.
On non-NTFS volumes the probe returns `Ok(false)` immediately.

The warning is emitted via the standard `tracing::warn!` channel,
which the logging crate routes to stderr. This matches how other
transfer-time warnings (vanished files, permission denials) are
surfaced.

## 5. Feature gating

No new Cargo feature is required. The code paths are gated by
`#[cfg(windows)]` / `#[cfg(not(windows))]` at the module level,
matching the existing pattern in `copy_file_ex.rs`, `refs_detect.rs`,
and `mmap_reader_stub.rs`.

The `has_named_ads` stub on non-Windows compiles to a no-op that
returns `Ok(false)`, so callers do not need `#[cfg]` at the call
site.

## 6. Integration points

### 6.1 Transfer pipeline

```
cli
 --> core::session()
      --> engine (generator / flist walker)
           --> fast_io::ads_detect::has_named_ads()  [probe]
           --> tracing::warn!()                       [one-shot]
      --> metadata::xattr::apply_xattrs_from_wire()
           --> metadata::xattr_windows::write_attribute()
                --> strip_ads_prefix()                [prefix guard]
                --> stream_path_wide()
                --> CreateFileW()
```

### 6.2 Crate dependencies

- `fast_io` already depends on `windows-sys` for Windows targets.
  No new dependency.
- `engine` already depends on `fast_io`. No new dependency.
- `metadata` has no new dependency (prefix-strip is pure byte
  manipulation).

### 6.3 Existing CI coverage

The `windows-acl-xattr` CI job already exercises `FindFirstStreamW`
and `stream_path_wide` on every push. The new `has_named_ads` function
reuses the same Win32 surface and is covered by the same job.

## 7. Test strategy

### 7.1 Unit tests in `fast_io::ads_detect`

| Test | Assertion |
|------|-----------|
| `has_named_ads_returns_false_for_no_streams` | File with no ADS returns `Ok(false)` |
| `has_named_ads_returns_true_for_named_stream` | File with a written ADS returns `Ok(true)` |
| `has_named_ads_returns_false_on_non_ntfs` | FAT32/exFAT volume returns `Ok(false)` (graceful) |
| `has_named_ads_stub_returns_false` | Non-Windows stub always returns `Ok(false)` |

Windows-only tests use the same `ads_supported()` guard pattern from
`xattr_windows.rs` to skip gracefully on non-NTFS CI runners.

### 7.2 Unit tests in `metadata::xattr_windows`

| Test | Assertion |
|------|-----------|
| `strip_ads_prefix_removes_prefix` | `user.windows.ads.Zone.Identifier` -> `Zone.Identifier` |
| `strip_ads_prefix_leaves_bare_name` | `Zone.Identifier` -> `Zone.Identifier` (no-op) |
| `strip_ads_prefix_leaves_other_prefix` | `user.something` -> `user.something` (no-op) |
| `write_attribute_strips_prefix` | Round-trip: write with prefixed name, read with bare name succeeds |

### 7.3 Integration test (deferred to WPC-4)

WPC-4 (#2906) owns the end-to-end regression test that confirms:

- Windows-to-Windows ADS round-trip via `-X`.
- Windows-to-Linux ADS preservation as `user.windows.ads.*` xattrs.
- Linux-to-Windows reconstitution from `user.windows.ads.*` xattrs
  back to bare NTFS ADS.
- Warning emission when `-X` is absent and source has ADS.

### 7.4 Man-page verification

Manual review that the EXTENDED ATTRIBUTES section documents:

- `-X` surfaces every named NTFS data stream as a
  `user.windows.ads.<streamname>` xattr entry.
- Default `-a` matches upstream rsync on Cygwin by ignoring ADS.
- Non-NTFS destinations (FAT32, exFAT) will fail to apply ADS on
  write.

## 8. Files changed

| File | Change |
|------|--------|
| `crates/fast_io/src/ads_detect.rs` | New. `has_named_ads()` + stub. |
| `crates/fast_io/src/lib.rs` | Add `pub mod ads_detect;` and re-export. |
| `crates/metadata/src/xattr_windows.rs` | Add `ADS_XATTR_PREFIX`, `strip_ads_prefix()`, call in `write_attribute`. |
| `crates/engine/src/` (generator walker) | One-shot ADS warning with `AtomicBool` guard. |
| Man page source | EXTENDED ATTRIBUTES section update. |

## 9. Acceptance criteria

Restated from WPC-2 section 4, scoped to this implementation:

1. `has_named_ads()` correctly detects ADS on NTFS and returns
   `false` on non-NTFS/non-Windows.
2. `strip_ads_prefix()` strips the `user.windows.ads.` prefix on the
   write path so cross-platform round-trips reconstitute bare ADS
   names.
3. One-shot warning fires exactly once per transfer when all four
   conditions (Windows, ADS present, no `-X`, not yet warned) hold.
4. All unit tests pass on Windows CI runners.
5. No new Cargo features, no new wire frames, no new capability bits.

## 10. Cross-references

- WPC-1 audit: `docs/audit/windows-ads-handling.md` (#2903).
- WPC-2 strategy: `docs/design/windows-ads-strategy.md` (#2904).
- WPC-4 regression test: #2906 (pending).
- Parent: #2869 (Windows real-world parity series).
- Existing code: `crates/metadata/src/xattr_windows.rs`,
  `crates/metadata/src/xattr.rs`.
- CI: `docs/design/windows-acl-xattr-ci-matrix.md`.
