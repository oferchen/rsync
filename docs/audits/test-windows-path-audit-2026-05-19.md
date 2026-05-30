# Windows hard-coded Unix path audit (2026-05-19)

## Scope

Follow-up audit to PRs #4549 (flist edge_cases) and #4551 (daemon config_parsing
tests) which fixed two test files that hard-coded Unix-style absolute paths and
then asserted equality or relied on the path being treated as absolute. Such
tests fail on Windows because `Path::is_absolute("/x")` returns `false` on
Windows (no drive letter or UNC prefix).

This audit searches the workspace for additional latent instances of the same
pattern.

## Search strategy

Patterns inspected:

- `PathBuf::from("/...")` used as the expected value in `assert_eq!`.
- `Path::new("/...")` followed by `.is_absolute()` assertions.
- String literals containing `/etc/`, `/tmp/`, `/var/`, `/usr/`, `/home/`,
  `/opt/` in test fixtures, especially in `assert_eq!` or values that flow
  through canonicalisation, `is_absolute`, or rooted-`join` logic.
- `set_var(..., "/...")` and similar env-var-with-Unix-absolute-path patterns.

Targets:

- `crates/**/tests/`
- `crates/**/src/**/tests.rs`
- `crates/**/src/**/tests/`
- `crates/**/src/**/*` (inline `#[cfg(test)]` modules)

Excluded per task constraints:

- `crates/daemon/src/daemon/sections/config_parsing/` (fixed in #4551).
- `crates/flist/tests/edge_cases.rs` (fixed in #4549).
- `crates/engine/src/concurrent_delta/` (PR #4552 in flight).

## Findings

No new latent bugs found beyond what is already in flight.

### Why the remaining matches do not break Windows

Every other match falls into one of three buckets:

1. **Already `#[cfg(unix)]`-gated.** Examples (representative, not exhaustive):
   - `crates/engine/src/local_copy/options/link_dest.rs` (file-level `#[cfg(unix)]`
     on the absolute-path tests).
   - `crates/engine/src/local_copy/dir_merge/load.rs` (`#[cfg(unix)]` on
     `resolve_dir_merge_path_*` tests).
   - `crates/engine/src/local_copy/tests/execute_symlink_edge_cases.rs`
     (broken-symlink tests are entirely `#[cfg(unix)]`).
   - `crates/engine/tests/delete_determinism_property.rs` (file-level
     `#![cfg(unix)]`).
   - `crates/transfer/tests/symlink_preservation.rs` (file-level `#![cfg(unix)]`).
   - `crates/daemon/src/daemon/sections/module_definition.rs` declares the
     tests module as `#[cfg(all(test, unix))]`; the test bodies use `/data`,
     `/etc/secrets`, etc. but never run on Windows.
   - `crates/daemon/src/daemon/sections/server_runtime/tests.rs`
     `reload_config_*` tests carry `#[cfg(unix)]`.

2. **Path used purely as a string / structural `PathBuf`.** PathBuf equality
   compares iterator-of-components, so on Windows `PathBuf::from("/tmp/foo") ==
   PathBuf::from("/tmp/foo")` regardless of whether the path is "absolute" in
   the Windows sense. Tests that store a path and read it back unchanged are
   portable. Examples:
   - `crates/transfer/src/pipeline/pending.rs` (`new_full_transfer` round-trip).
   - `crates/transfer/src/pipeline/async_dispatch.rs::make_dest_path_joins_correctly`
     (uses `Path::join`; components compare equal across `/` and `\`).
   - `crates/transfer/src/receiver/tests/errors_and_timeouts/mod.rs`
     (`stats.metadata_errors` round-trip).
   - `crates/transfer/src/generator/tests.rs::resolve_*` (input PathBuf joined
     with a relative; component iteration is identical on both platforms).
   - `crates/protocol/src/error_recovery/mod.rs` (`PartialTransferLog`
     store-and-retrieve).
   - `crates/core/src/client/summary/event.rs::resolve_destination_path_*`
     (`Path::join` only).
   - `crates/core/src/client/config/reference.rs` (`ReferenceDirectory::new`
     stores and returns the path unchanged).
   - `crates/core/src/client/config/enums/files_from.rs::FilesFromSource`
     constructor predicates.
   - `crates/cli/src/frontend/arguments/tests.rs::tmp_dir_alias` (parses
     `--tmp-dir=/tmp/test`, asserts on the parsed `PathBuf` round-trip).
   - `crates/cli/src/frontend/tests/parse_args_recognises_temp.rs` (same
     pattern with `-T`).
   - `crates/engine/src/local_copy/context.rs` (`CreatedEntry` clone test).
   - `crates/engine/src/local_copy/error.rs::local_copy_error_kind_as_io`.
   - `crates/engine/src/local_copy/options/{staging,backup,logging,types}.rs`
     (builder setters return what was set).
   - `crates/engine/src/local_copy/plan/plan_impl.rs` (plan `destination()`
     round-trip).
   - `crates/engine/src/local_copy/hard_links/tests/apply_tracker_tests.rs`
     (tracker leader/deferred round-trip; the live disk operations are
     `#[cfg(unix)]`-gated separately).
   - `crates/engine/src/local_copy/executor/reference.rs` (component-join is
     deterministic on both platforms because rooted second operands replace the
     cwd-relative portion of the first on Windows; result is byte-identical to
     Unix).
   - `crates/engine/src/local_copy/executor/file/backup.rs` (`compute_backup_path`;
     `dir.is_absolute()` differs on Windows but the resulting `join` collapses
     to the same components either way).
   - `crates/engine/src/local_copy/executor/file/partial.rs::partial_mode_*`
     (only inspects the `PartialMode` variant; the inner `PathBuf` is never
     interpreted as an absolute filesystem path).
   - `crates/engine/src/local_copy/executor/file/paths.rs` (asserts use
     `to_string_lossy().contains(...)` on the prefix/file-name; no
     `is_absolute` dependency).
   - `crates/engine/src/local_copy/operands.rs` (`SourceSpec`/`DestinationSpec`
     component iteration is platform-stable).
   - `crates/engine/src/local_copy/tests/execute_log_file.rs` (option builder
     round-trip; the `log_file_transfer_*` integration tests use `tempdir`).
   - `crates/engine/src/local_copy/filter_program/rules.rs::exclude_if_present_marker_path_absolute`
     (Windows reaches the same destination because joining with `/etc/nobackup`
     replaces the cwd-relative portion of `/home/user/docs`).
   - `crates/daemon/src/daemon/sections/variable_expansion.rs` (variable
     expander is pure string substitution; PathBuf equality is structural).
   - `crates/daemon/src/daemon/sections/module_definition/tests.rs` (entire
     module is `#[cfg(all(test, unix))]` via `module_definition.rs:14`).
   - `crates/rsync_io/src/ssh/embedded/config.rs::builder_identity_files`
     (`Vec<PathBuf>` round-trip).
   - `crates/flist/src/tests.rs::error_*` (error path round-trip; `Display`
     contains literal substring).
   - `crates/filters/tests/{exclude_from_file_parsing,filter_rule_syntax}.rs`
     (error-on-nonexistent-file tests assert `is_err()` and that the error
     message contains a filename substring; both hold on Windows).

3. **Tests that drive real filesystem paths via `tempfile::TempDir`.**
   `tempdir().path()` is always absolute on every platform, so any subsequent
   `is_absolute()` or `canonicalize` assertion is portable.
   - `crates/flist/src/entry.rs::full_path_returns_absolute`.
   - `crates/flist/src/file_list_walker.rs::absolutize_*` (the Windows variant
     is explicitly `#[cfg(windows)]`).
   - `crates/flist/tests/{incremental_processing,traversal_options,directory_tree_building,path_max_limits}.rs`
     (walkers driven by `tempdir`).
   - `crates/core/src/message/tests/part{1,2,7}.rs::*absolute*` (sources are
     built from `CARGO_MANIFEST_DIR` and `RSYNC_WORKSPACE_ROOT`, both absolute
     on every platform; the only literal `/tmp/outside.rs` flows through
     `Path::join(manifest_dir, ...)` which produces `C:\\...\\tmp\\outside.rs`
     on Windows and is therefore absolute).
   - `crates/engine/src/local_copy/tests/execute_temp_dir.rs::*absolute_staging*`
     (uses `tempdir().canonicalize()`).

### Out-of-scope observation

The remaining concentration of `PathBuf::from("/srv/docs")`-style fixtures
lives in `crates/daemon/src/tests/chunks/` (roughly 50 chunk files that build
inline daemon configs with `path = /srv/docs` and similar). Two facts make
these out of scope for this audit:

1. **Daemon is not exercised on Windows in current CI.** `windows-test` runs
   only `core`, `engine`, and `cli`; `windows-gnu-cross-check` performs
   `cargo check --workspace` without `--tests`, which never compiles the
   `daemon::tests` module. The fixtures cannot fail on Windows today because
   the cell that would run them does not exist.

2. **The closest gating decision already lives in the codebase.** When the
   `daemon::sections::module_definition` test module was split out, the
   maintainer chose `#[cfg(all(test, unix))]` rather than introducing
   per-fixture absolute-path helpers. Mirroring that decision for the rest of
   `daemon::tests` is a CI-policy change rather than a bug fix and belongs in
   a separate PR if and when Windows daemon coverage is added.

## Conclusion

No fixes required. PRs #4549 and #4551 closed the only instances of the
pattern in crates currently exercised on Windows CI. Future Windows expansion
that adds the `daemon` crate to the windows-test matrix should pre-emptively
gate `crates/daemon/src/tests.rs` (or migrate the chunk fixtures to a
platform-aware `abs("/...")` helper as in #4551) before enabling the matrix
cell.
