# Post-Session Master Clippy / MSRV Debt Audit

Snapshot of `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings`
against `origin/master` after the mass-merge cascade. Rust toolchain is pinned to
`1.88.0` in `rust-toolchain.toml`, so the same run doubles as the MSRV check.

This is a pure inventory. No source changes were made. Severity rankings:

- P0: hard compile error - breaks `-D warnings` CI on at least one platform.
- P1: clippy-only lint promoted to error by `-D warnings`.
- P2: informational warning (cfg conditions, doc style).

## Already fixed in prior PRs

Master already shipped the following fixes that earlier sessions flagged:

- `fix(fast_io): import FileReader trait for IoUringFileReader::open` (PR #4452,
  commit `8ab66e6a5`) - resolved the missing trait import on the io_uring file
  reader open path.
- `fix(fast_io): clippy compliance in nvme_data_path bench` (PR #4454, commit
  `c2b0d94d2`) - addressed doc-quote and format-string lints in the benchmark.

The lints listed below were not covered by either PR.

## Remaining lint failures

### P0 - hard compile errors

#### 1. `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:122` (and `:133`, `:140`)

Lint: `E0596` (cannot borrow as mutable, immutable binding).

Three identical occurrences in the synchronous `io::Read` / `io::Write` bridge
over the embedded SSH runtime. The bindings produced by
`self.stream.as_mut()` are not declared `mut`, but `AsyncRead::read`,
`AsyncWriteExt::write_all`, and `AsyncWriteExt::flush` all require `&mut Self`.

```text
error[E0596]: cannot borrow `stream` as mutable, as it is not declared as mutable
   --> crates/rsync_io/src/ssh/embedded/sync_bridge.rs:122:44
    |
122 |         self.runtime.block_on(async move { stream.read(buf).await })
```

Recommended fix: declare each `stream` binding `let mut stream = self.stream.as_mut();`
in the three impl methods (`read`, `write`, `flush`). Triggers only with the
`embedded-ssh` feature on.

#### 2. `crates/fast_io/src/sendfile.rs:881`

Lint: `E0382` (borrow of moved value `expected_content`).

The test `expected_content` `Vec<u8>` is moved into the reader thread closure on
line 851, then re-borrowed on line 881 for an `assert_eq!` length comparison.

Recommended fix: stash the expected length in a `usize` before the spawn, or
clone the `Vec` again before the closure consumes it. macOS-only test target.

#### 3. `crates/engine/src/concurrent_delta/parallel_apply.rs:739`

Lint: `E0277` (`dyn Write + Send` does not implement `Debug`).

`expect_err("slot is still referenced by leaked clone")` requires `T: Debug` on
the `Ok` branch payload. The current `Result<Box<dyn Write + Send>, _>` carries
a non-`Debug` value.

Recommended fix: pattern-match with `assert!(matches!(result, Err(_)))` instead
of `.expect_err(...)`, or store an opaque marker type in the `Ok` arm for the
test.

#### 4. `crates/engine/src/delete/context.rs:872` and `:893`

Lint: `E0277` (`DeleteEmitter<RecordingDeleteFs>` and `DrainOutcome<...>` do
not implement `Debug`).

Same root cause as item 3.

Recommended fix: derive or hand-implement `Debug` on `DeleteEmitter` and
`DrainOutcome` (both test-helper types), or replace `.expect_err(...)` with a
`matches!`-based assertion. The audit prefers the `derive(Debug)` route since
both types are recorder/observer structs already trivial to debug-print.

#### 5. `crates/engine/src/concurrent_delta/consumer.rs:497` (and three siblings)

Lint: `E0004` (non-exhaustive `match`).

The `match` on `reorder.insert(...)` covers `Ok(())` and
`Err(SpillError::Capacity(_))` but does not bind the new `SpillError::Io(_)`
variant introduced by the spill error-type refactor. Three additional sites in
`crates/engine/src/concurrent_delta/spill/mod.rs:1435` and the test module hit
the same gap when expanding `SpillError` patterns.

Recommended fix: add the missing `SpillError::Io(_)` arm in each match (the
spill module test in particular should re-export it through the local error
wildcard). The consumer hot loop must propagate `Io(_)` as a fatal failure,
not silently retry as it does for `Capacity`.

#### 6. `crates/engine/src/concurrent_delta/spill/mod.rs:1755` and `:1781`

Lint: `E0425` (cannot find `tempfile` in scope).

Test-only import of `tempfile::tempdir` is missing under the
`spill-compression` feature path; the symbol is referenced but never imported
in the `#[cfg(feature = "spill-compression")]` block.

Recommended fix: add `use tempfile;` (or the explicit
`use tempfile::tempdir;`) inside the test module so both
`compression_none_writes_uncompressed_tag` and
`compression_zstd_writes_compressed_tag` compile.

#### 7. `crates/fast_io/src/kqueue/mod.rs:40`

Lint: `duplicated attribute`.

`#![cfg(target_os = "macos")]` is set both on the module file
(`kqueue/mod.rs:40`) and on the `pub mod kqueue;` declaration in
`crates/fast_io/src/lib.rs:177`. The inner attribute is redundant and clippy
escalates the duplication to a warning that fails under `-D warnings` (counts
3x because every macOS build target re-emits the diagnostic).

Recommended fix: drop the `#![cfg(target_os = "macos")]` from
`crates/fast_io/src/kqueue/mod.rs` and rely on the outer `#[cfg(...)]` in
`lib.rs`.

#### 8. `crates/fast_io/src/macos_io.rs:336`

Lint: `clippy::borrow_deref_ref`.

```text
let mut file_ref = &*file;
```

`&*file` reborrows an immutable reference; clippy flags it as a no-op.

Recommended fix: rebind directly with `let mut file_ref = file;` or omit the
intermediate altogether and call `file.write_all(&buf[remaining..])?;` on the
caller-provided reference.

### P1 - clippy-only lints promoted to errors

#### 9. `crates/engine/src/concurrent_delta/strategy.rs:358`

Lint: `unreachable_code`.

A `let _ = len;` "silence unused-variable" line lives after the
`#[cfg(not(...))]` block that already returns; clippy reports the trailing
statement as unreachable on the `iouring-data-reads`-on configuration.

Recommended fix: collapse the cfg-gated silencer into a single `let _ = len;`
at the top of the function (before the cfg-on branch) so both feature
permutations consume the binding exactly once.

#### 10. `crates/engine/src/concurrent_delta/spill/mod.rs:62`

Lint: `unused_imports` (`Seek`).

`std::io::Seek` is imported alongside `Read`, `SeekFrom`, and `Write`, but no
call site uses the trait directly after the recent `BufReader`-driven rewrite.
Counts twice because the lint fires once per feature column.

Recommended fix: drop `Seek` from the import list. `SeekFrom` is still used and
must stay.

#### 11. `crates/engine/src/local_copy/buffer_pool/page_aligned.rs:30`

Lint: `unused_imports` (`page_size`).

Imported from `fast_io` along with `PageAlignedBuffer` and `round_up_to_page`,
but no longer referenced after the page-rounding logic moved into
`round_up_to_page`.

Recommended fix: drop `page_size` from the brace-import list.

### P2 - informational warnings

#### 12. `crates/fast_io/benches/nvme_data_path.rs:22-28` (7 occurrences)

Lint: `clippy::doc_list_item_without_indentation`.

The module-level doc comment uses bullet items whose continuation lines start
in column 4 instead of column 5, so clippy parses the trailing prose as a
separate top-level paragraph. Repeats once per bullet.

Recommended fix: re-indent the wrapped lines of each `-` bullet to align under
the bullet text (one extra space). This is purely cosmetic - the bench still
runs - but the lint fails under `-D warnings` when clippy targets benches.

#### 13. `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:958` (and `:967`, `:983`, +1 more)

Lint: `unexpected_cfg_condition_value` (`iouring-data-writes`).

The engine source gates four items on
`#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]`, but
`crates/engine/Cargo.toml` only declares `iouring-data-reads` in its
`[features]` table. `fast_io` does declare `iouring-data-writes`; engine
either needs its own pass-through feature
(`iouring-data-writes = ["fast_io/iouring-data-writes"]`) or the cfg gate must
be rewritten to reference the upstream crate directly.

Recommended fix: add a pass-through feature flag in
`crates/engine/Cargo.toml` that re-exports
`fast_io/iouring-data-writes`, mirroring the existing
`iouring-data-reads` line, and keep the existing cfg gates.

## MSRV (`1.88.0`) status

The workspace toolchain is pinned to `1.88.0`, so the clippy run above is the
MSRV check. No additional MSRV-only regressions surfaced beyond the items
listed above. All P0 errors above reproduce identically on `1.88.0`; none of
them depend on a newer language or library feature.

## Summary tally

- P0 hard errors: 8 distinct lint clusters, ~22 individual diagnostics.
- P1 clippy errors: 3 distinct clusters (5 diagnostics counting duplicates).
- P2 informational: 2 clusters (11 diagnostics counting duplicates).
- MSRV-specific regressions: none.

CI status reproduces locally with the pinned toolchain. The
`embedded-ssh`-gated and `iouring-data-writes`-gated regressions account for
half the P0 cluster and merit prioritisation because they hide behind
non-default feature combinations.
