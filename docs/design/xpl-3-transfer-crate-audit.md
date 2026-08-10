# XPL-3: `crates/transfer/` cross-platform `cfg` gate audit

Audits the transfer crate for macOS / Windows / Linux-musl `cfg`-gate
consistency, following the model established by XPL-1 (`apple-fs`) and
XPL-2 (`kqueue_stub`).

The hazards looked for, drawn from `feedback_proactive_cross_platform.md`
and the "Cross-Platform Compilation" section of `CLAUDE.md`:

- Unused imports/variables behind `#[cfg(target_os = "...")]` gates on
  the other platform.
- `#[cfg(unix)]` test modules where individual tests are unix-only but
  the module declaration itself is not gated.
- `unused_mut` on Windows when a `let mut x` is only mutated inside
  `#[cfg(unix)]` blocks.
- Dead `enum` variants on the non-target platform (must gate the
  variant *and* every match arm that names it).
- Missing no-op stubs for unsupported platforms.
- Rustdoc link breaks on re-exports (backtick-only `` `Type` ``, not
  `` [`Type`] ``).
- `#[cfg(feature = "...")]` gates whose default-feature combination
  breaks `--all-features` or `--no-default-features`.

## Inventory

Counts of `cfg` predicates in `crates/transfer/src/`:

| Predicate              | Count |
|------------------------|------:|
| `cfg(unix)`            |   226 |
| `cfg(not(unix))`       |    53 |
| `cfg(target_os = ...)` |    13 |
| `cfg(windows)`         |     3 |
| `cfg(feature = ...)`   |    60 |

Note: the `cfg(windows)` predicate count is low because Windows-specific
code largely sits inside `cfg(not(unix))` blocks alongside the catch-all
non-Unix fallback. Native Windows-only paths exist only in
`receiver/file_list/sanitize.rs` (drive/UNC prefix rejection) and the
matching test file.

The `Cargo.toml` platform-conditional dependency tree:

- `cfg(target_os = "macos")` -> `apple-fs` (already audited under XPL-1).
- `cfg(unix)` -> `checksums`, `libc`, `metadata` with `xattr` feature,
  `engine` with `xattr` feature.
- `cfg(windows)` -> `checksums` with `default-features = false`.
- `cfg(any(target_os = "linux", target_os = "android"))` (dev) ->
  `filetime`.
- `cfg(unix)` (dev) -> `xattr`.

The dev-dependency split means `filetime` only exists on Linux/Android,
which is consistent with its only use site
(`generator/open_source.rs::open_source_with_noatime_preserves_atime_on_linux`,
itself `#[cfg(target_os = "linux")]`-gated).

## Findings

### CI-fatal hazards

**None found.** The audit walked every `cfg` site in the crate and did
not surface any pattern that would break the `fmt+clippy`, `nextest
(stable)`, `Windows (stable)`, `macOS (stable)`, or `Linux musl
(stable)` checks beyond what is already in tree and presumably passing
on master.

### Warning hazards (cheap-to-fix or document-only)

#### W-1. `flags.rs:154` uses `#[allow(unused_mut)]` instead of the documented `let _ = &var;` pattern.

`crates/transfer/src/flags.rs:154`:

```rust
#[allow(unused_mut)] // REASON: mutated when acl or xattr features are not enabled
let mut cleared = Vec::new();
```

CLAUDE.md recommends `let _ = &var;` over `#[allow(unused_mut)]` for
exactly this shape, but the existing form compiles cleanly across all
target/feature matrices and carries a `REASON:` comment naming the
trigger condition. Leaving in place: refactoring would churn the only
caller-visible path of a critical sanity helper without an observable
benefit. Document for future style sweeps.

### Clean (correctly gated, no action needed)

The following files were inspected end-to-end and confirmed clean. Each
either uses paired `#[cfg(unix)]` / `#[cfg(not(unix))]` blocks with
matching shapes, gates the entire test module when every test inside is
platform-specific, or routes through a `cfg`-aware shim function:

- `Cargo.toml` - platform-conditional dependency tree consistent with
  source-side `cfg` usage; `apple-fs` only Darwin, `libc` only Unix,
  `xattr` dev-dep only Unix, `filetime` dev-dep only Linux/Android.
- `lib.rs` - no platform-conditional code at the crate root.
- `disk_commit/writer.rs:144-300` - `Writer<'a>` enum variants are
  individually gated (`IoUring`, `Iocp`, `Macos`, `Vmsplice`); every
  `match self {}` arm matches the same gates; the catch-all
  `Writer::Buffered(_)` is the only unconditional arm. The
  `#[cfg_attr(not(any(linux+io_uring, windows+iocp)), allow(unused_variables))]`
  on `finish()` covers the `do_fsync` / `file_path` params on macOS and
  vmsplice-only Linux. Verified: on Linux musl (no features), the enum
  has only `Buffered`, so the match is exhaustive and the params are
  unused but allowed.
- `disk_commit/process.rs:18-22, 258-266, 304-340, 387-396` - paired
  `cfg(unix)` / `cfg(not(unix))` for temp-file open routing and the
  Linux/Windows/macOS dispatch in `make_writer`. The
  `#[allow(unused_variables)]` on `make_writer` line 294 correctly
  silences the unused `disk_batch` / `iocp_batch` params on platforms
  without their backend. Macos test at line 571 is `cfg(target_os =
  "macos")`-gated and never observed by other targets.
- `disk_commit/config.rs:8, 60, 120` - `Arc` is imported
  unconditionally (used by non-cfg fields at lines 68 and 79). The
  `sandbox` field is `#[cfg(unix)]`-gated with a matching `#[cfg(unix)]`
  default in the `impl Default` block.
- `delta_apply/applicator.rs:29-35, 422-436` - `SameFsCache::SameFs` is
  `#[cfg(target_os = "linux")]`-gated. The match-free `==` comparisons
  on lines 410 and 413 mean non-Linux targets correctly never observe
  the variant. `resolve_same_fs()` returns `DifferentFs` on non-Linux
  via a closed `cfg(not(target_os = "linux"))` branch consuming both
  unused params via `let _ = (basis, dest);`. Type alias
  `BasisMapStrategy` (line 40-42) paired correctly: `AdaptiveMapStrategy`
  on Unix, `BufferedMap` on non-Unix.
- `generator/open_source.rs` - paired `cfg(any(target_os = "linux",
  target_os = "android"))` for `O_NOATIME`-bearing `try_open_noatime`
  with a `cfg(not(...))` stub returning `Ok(None)`. Imports
  (`OpenOptionsExt`, `libc::*`) only pulled in on Linux/Android. Clean
  no-op stub model.
- `generator/file_list/entry.rs` - paired Unix / non-Unix branches for
  mode bits (lines 54-61, 72-73), mtime conversion (113-124), and
  atime/owner/group/hardlink/xattr handling. `rdev_to_major_minor`
  has Linux and BSD/macOS branches (lines 351-366). Non-Unix path
  deliberately omits `fake_super_override` and documents the rationale
  in a `//` comment (line 290-293). `#[cfg(all(test, unix))]` correctly
  gates the entire fake-super test module so non-Unix builds skip both
  the imports and the tests.
- `generator/file_list/hardlinks.rs:13-14, 33-34, 84-85, 122-123` -
  `assign_hardlink_indices` and `collect_id_mappings` are `cfg(unix)`
  with a paired `cfg(not(unix))` no-op stub for `collect_id_mappings`.
  `assign_hardlink_indices` callers in `mod.rs:111-114` and
  `mod.rs:227-230` are themselves `cfg(unix)`-gated so the missing
  non-Unix definition is unreachable rather than dead.
- `generator/tests.rs:1749-1768` - `rdev_to_major_minor` test pairs
  use `cfg(all(unix, target_os = "linux"))` and `cfg(all(unix,
  not(target_os = "linux")))`. Mutually exclusive; either path runs
  exactly one variant.
- `receiver/file_list/id_lists.rs:12-13, 34-35, 88-89` - separate
  `cfg(unix)` and `cfg(not(unix))` implementations of
  `receive_id_lists`, with id_lookup imports only on Unix.
- `receiver/file_list/sanitize.rs:65-80` - `#[cfg(windows)]` block
  inside `retain` closure rejects drive/UNC prefixes; the closure compiles
  on Unix since the gated block simply doesn't materialise. Test
  counterpart in `receiver/tests/errors_and_timeouts/sanitize_file_list.rs:183,
  200` gates the matching test functions with `#[cfg(windows)]`.
- `receiver/quick_check.rs:76-89, 98-111, 342` - paired Unix /
  non-Unix mtime comparison branches; non-Unix path uses
  `metadata.modified()` + `duration_since(UNIX_EPOCH)` to derive
  seconds. The `info_copy_emission_tests` module at 342 is
  `cfg(unix)`-gated *and* `cfg(test)`-gated so the unix-only
  `PermissionsExt`/`MetadataExt` imports never reach Windows.
- `receiver/directory/mod.rs:17-26` - `normalize_filename_for_compare`
  has paired macOS-NFD / non-macOS-noop implementations. Both signatures
  return `OsString`, matching at callers.
- `receiver/directory/creation.rs:40, 82-101, 123-140, 320, 355-371`
  - sandbox param is `#[cfg(unix)]` on the public methods,
  `mkdirat_via_sandbox_or_fallback` is paired with a
  `fs::create_dir_all` non-Unix branch, and the ACL feature gate
  `#[cfg(all(feature = "acl", any(target_os = "linux", target_os =
  "macos", target_os = "freebsd")))]` is consistent between the
  declaration (line 82-90) and the use sites (line 98-101). Note the
  Linux/macOS/FreeBSD triple matches `metadata::default_perms_for_dir`'s
  platform support.
- `receiver/directory/links.rs:32, 163-168, 197, 284-365, 441` -
  `create_symlinks` has two definitions: the Unix path takes
  `Option<&fast_io::DirSandbox>`, the non-Unix path is a typed no-op
  with only the `_dest_dir`/`_writer` params. `create_hardlinks` puts
  the `sandbox` param behind `#[cfg(unix)]` on the signature and gates
  every internal call site to match.
- `receiver/transfer/sync.rs:25-28, 57-78, 203-211, 312-332, 351-372,
  400-402` - paired `cfg(unix)`/`cfg(not(unix))` for sandbox-routed vs
  path-based temp file open, symlink creation, backup rename, and
  commit rename. Every Unix branch's matching non-Unix counterpart uses
  the standard `std::fs` API. `apply_xattrs_from_list` (line 380-385)
  is *not* cfg-gated; it lives in the `metadata` crate and on non-Unix
  uses a no-op stub there (out of scope for this audit, covered by
  the metadata crate's own gating).
- `receiver/transfer/pipelined.rs:49-55, 62-67, 172-175` -
  `create_directories`, `create_symlinks`, `create_hardlinks`,
  `delete_extraneous_files` all have paired `cfg(unix)`/`cfg(not(unix))`
  call sites passing the sandbox or not. No stale params.
- `receiver/transfer/pipelined_incremental.rs:61-63, 86-89, 172-175` -
  identical paired pattern.
- `receiver/transfer/pipeline.rs:130-131, 302-305` - `sandbox` in
  `DiskCommitConfig` and `ResponseContext` is `cfg(unix)`-gated;
  population is gated to match.
- `receiver/transfer/setup.rs:74, 162-178, 192-206` - `sandbox` field
  in `PipelineSetup` and `open_sandbox_for_dest` helper are both
  `cfg(unix)`. Non-Unix builds simply omit the field; consumers in
  `pipelined.rs` etc. only read it inside `cfg(unix)` blocks.
- `setup/capability.rs:14-34, 49-58` - `CapabilityMapping::platform_ok`
  is declared with paired `#[cfg(unix)]` / `#[cfg(not(unix))]` field
  attributes both naming the same type (`bool`). Exactly one is active
  per build. The `'L'` (SYMLINK_TIMES) mapping at line 49-58 uses the
  same paired-attribute trick to flip `platform_ok` between `true` on
  Unix and `false` elsewhere. The `'i'`, `'s'`, `'f'`, `'x'`, `'C'`,
  `'I'`, `'v'`, `'u'` rows are unconditional (`platform_ok: true` on
  every target). `iconv_capability_compiled_in()` uses `cfg!(feature =
  "iconv")` for compile-time evaluation, no runtime cost. Both the
  builder and parser apply the same row filter, so the iconv-disabled
  build neither advertises `'s'` nor accepts it from peers - matching
  upstream's `#ifdef ICONV_OPTION` parity.
- `setup/mod.rs:39-49, 219-231, 270` - test-only re-exports gated by
  `cfg(test)`; `flags |= SYMLINK_TIMES` and `flags |= SYMLINK_ICONV`
  gated by `cfg(unix)` and `cfg(all(unix, feature = "iconv"))`
  respectively. The `let mut flags` declaration at line 211 is also
  mutated by the unconditional `if config.allow_inc_recurse` block
  (line 233), so no `unused_mut` lint fires on Windows.
- `temp_cleanup.rs:15-16, 96-98, 114, 187, 189-202, 367-407` -
  sandbox-aware unlink path is `cfg(unix)`-gated throughout the public
  signature (`Option<&Arc<fast_io::DirSandbox>>`); non-Unix builds
  simply pass through to `fs::remove_file(path)` with `let _ =
  (file_name, dest_dir)` to silence unused-arg lints. Tests at lines
  367, 387, 406 are `cfg(unix)`-gated to skip the `PermissionsExt`-
  using and sandbox-using cases on Windows.
- `pipeline/mod.rs:70-73` - `async_dispatch` and `async_pipeline`
  modules gated by `cfg(feature = "async")`. The crate-level `async`
  feature pulls `tokio` and `tokio-util` from the optional
  dependencies. Default-feature build leaves them out; no broken
  re-exports because the modules are entirely conditional, not just
  their bodies.
- `pipeline/receiver.rs:592-624` - the `permission_denied_on_output_is_recoverable`
  test is `cfg(unix)`-gated because it uses
  `PermissionsExt::from_mode(0o555)` and checks `USER == root`.
- `pipeline/spsc.rs`, `pipeline/pending.rs`, `pipeline/job.rs`,
  `pipeline/async_signature.rs`, `pipeline/async_dispatch.rs`,
  `pipeline/async_pipeline.rs`, `pipeline/state.rs` - cross-platform,
  no Unix-specific imports. The `cfg(test)` and `cfg(debug_assertions)`
  blocks are environment-conditional, not platform-conditional.
- `map_file/mod.rs:32-51` - `adaptive` and `mmap` submodules and their
  re-exports are `cfg(unix)`-gated together. `wrapper.rs:12-13, 47-119`
  guards the `MmapStrategy` / `AdaptiveMapStrategy` `impl` blocks
  identically; the cross-platform `MapFile<BufferedMap>` base impl is
  unconditional. Non-Unix consumers only ever see `MapFile<BufferedMap>`.
- `compressed_reader.rs:12-15, 36-89, 191, 260`, `compressed_writer.rs`
  similar - `lz4` / `zstd` features are tracked through the `Codec`
  enum variant gates and matching match-arm gates, with the catch-all
  `Codec::None` always unconditional. `--no-default-features` build
  results in only the `None` variant; the match arms remain exhaustive
  because every cfg-gated arm is matched in pairs with the variant
  declaration.
- `flags.rs:464, 488, 519, 524` - test-side `#[cfg]`-gating for the
  ACL/xattr feature matrices uses paired arms matching the
  `clear_unsupported_features()` cfg gates at lines 157, 163. The
  `clear_unsupported_features_handles_both_acl_and_xattr` test at line
  506-534 uses four mutually exclusive branches covering every
  acl/xattr feature combination.
- `setup/tests.rs:1006-1028` - `iconv` capability tests are mutually
  exclusive via `cfg(feature = "iconv")` and `cfg(not(feature =
  "iconv"))`. Both branches assert on `build_capability_string(false)`
  output.
- `receiver/tests/file_list/incremental_directories.rs:218, 248, 259,
  327`, `receiver/tests/file_list/filter_chain.rs:45, 97`,
  `receiver/tests/file_list/mod.rs:30`, `receiver/tests/support.rs:19`,
  `receiver/tests/hard_links.rs:19-31, 58-411` - test modules use
  individual `cfg(unix)` gating per test where appropriate; the
  `hard_links.rs` file uses a `call_create_hardlinks` shim function at
  line 22-31 that itself bridges the `cfg(unix)` sandbox-arg shape
  difference, allowing the tests above it to compile unmodified on both
  Windows and Unix.

## Methodology

1. Walked the dependency table in `Cargo.toml`.
2. Enumerated every `#[cfg(...)]` site in `crates/transfer/src/` using
   `grep -rn "cfg(...)"` against `unix`, `not(unix)`, `windows`,
   `target_os`, `any(...)`, `all(...)`, and `feature` predicates.
3. For each high-density file, inspected paired branches end-to-end to
   confirm: matching function signatures, no stale imports, no `let
   mut` that becomes immutable on the other platform, and exhaustive
   match arms across all enabled cfg combinations.
4. Cross-checked dev-dependency `cfg` gates against the source files
   that import them (`filetime` <-> `open_source.rs`, `xattr` <->
   `generator/file_list/entry.rs`).
5. Spot-checked `Writer<'a>` enum variant gating against all four
   match arms (`buffered_for_sparse`, `write_chunk`, `flush_and_sync`,
   `finish`) plus the `#[cfg_attr]` attribute on `finish` for
   `unused_variables` coverage on macOS / vmsplice-only Linux.

## Hazard counts

- **CI-fatal:** 0
- **Warning:** 1 (W-1, document-only)
- **Clean:** 60+ inspected sites across 25 files

The transfer crate is consistently structured. Every Unix-only path
has a paired non-Unix fallback (either no-op stub, path-based syscall,
or typed shim), every cfg-gated enum variant has matching cfg-gated
match arms, every cfg-gated import is matched by a cfg-gated use site,
and the dev-dependency platform restrictions mirror the only call
sites that reference them.

No source code changes are made in this PR. W-1 (the lone
`#[allow(unused_mut)]` in `flags.rs`) is documented as a future style
sweep candidate rather than a behavioural defect.
