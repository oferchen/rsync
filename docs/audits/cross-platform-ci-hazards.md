# Cross-platform CI hazard preflight

Audit of the workspace for three classes of issues that typically surface
only when CI runs on Windows or macOS, in advance of the next push.
Pure audit - no source changes proposed in this document; each row lists
the recommended fix for the owner of that file.

Audit performed on `origin/master`. Crate layout was scanned, not
compiled, so a few findings may be over-cautious; treat severity as a
prioritisation hint rather than an absolute.

Severity scale:

- HIGH - file is touched frequently or sits in a hot path (CLI, daemon,
  engine), so a Windows-only warning would block CI for many PRs.
- MED - file is touched periodically; warning shows up on most platform
  matrix runs.
- LOW - file is stable and rarely touched; the warning would be noisy
  but unlikely to block work.

## 1. Under-gated test modules

Files where every `#[test]` (or `#[test]` in scope) is individually
gated with `#[cfg(unix)]`, but the enclosing module is not. On Windows
the module compiles with zero `#[test]` functions, so any `use`
brought in for the tests becomes an `unused_import` warning -
promoted to a hard error under `-D warnings`.

Recommended fix in each row gates the file (or the `#[path = ...] mod`
inclusion in the parent `mod.rs`) so the module disappears wholesale on
Windows.

The daemon `tests/chunks/*.rs` files were inspected but excluded -
they are `include!()`-ed into `crates/daemon/src/tests.rs`, so the
`#[cfg(unix)]` tests share the parent module's imports and pose no
unused-import risk.

| File:line | Description | Recommended fix | Severity |
|-----------|-------------|-----------------|----------|
| crates/cli/src/frontend/tests/combined.rs:5 | Single `#[cfg(unix)]` test, file pulled in unconditionally via `#[path = "combined.rs"] mod combined_tests;` in `tests/mod.rs:46`. On Windows the file compiles with `use super::*;` but no tests; the glob may pull in items the parent module reports as unused. | Gate the `#[path]` inclusion in `tests/mod.rs` with `#[cfg(unix)]`, matching how `error_recovery.rs` is wired one line above (`tests/mod.rs:65`). | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_archive.rs:5 | Single Unix-only test (`-a` archive mode is Unix-specific). File pulled in unconditionally at `tests/mod.rs:301`. | Wrap the `#[path = "transfer_request_with_archive.rs"]` inclusion in `tests/mod.rs:301` with `#[cfg(unix)]`. | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_executability.rs:4 | Single Unix-only test (executability bit). Included unconditionally at `tests/mod.rs:311`. | Add `#[cfg(unix)]` above the `#[path = ...]` inclusion at `tests/mod.rs:311`. | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_omit.rs:5,48 | Two Unix-only tests (omit-link-times, omit-dir-times). Included unconditionally at `tests/mod.rs:331`. | Add `#[cfg(unix)]` above the inclusion at `tests/mod.rs:331`. | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_owner.rs:4 | Single Unix-only test (ownership preservation). Included unconditionally at `tests/mod.rs:335`. | Add `#[cfg(unix)]` above the inclusion at `tests/mod.rs:335`. | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_perms.rs:4 | Single Unix-only test (permission preservation). Included unconditionally at `tests/mod.rs:337`. | Add `#[cfg(unix)]` above the inclusion at `tests/mod.rs:337`. | HIGH |
| crates/cli/src/frontend/tests/transfer_request_with_sparse.rs:4,59,107,153,234,315 | Six Unix-only tests (sparse-write semantics rely on Unix sparse semantics). Included unconditionally at `tests/mod.rs:343`. | Add `#[cfg(unix)]` above the inclusion at `tests/mod.rs:343`. | HIGH |

Also noted while running this pass (not a duplicate of the table, but
worth surfacing to the same owner):

- `crates/daemon/src/tests/chunks/daemon_files_from_stdin_push.rs`
  is fully `#[cfg(unix)]`-gated yet does not appear in any
  `include!(...)` line in `crates/daemon/src/tests.rs`. The file is
  effectively orphaned. Either include it (under `#[cfg(unix)]`) or
  delete it. Severity: LOW; it just means the test never runs.

## 2. `let mut` bindings mutated only inside `#[cfg(unix)]`

Variables declared `let mut x = ...` outside any platform gate, where
the only mutations happen inside `#[cfg(unix)]` blocks. On Windows the
binding becomes effectively immutable, producing an `unused_mut`
warning that promotes to a hard error.

The scan looked for `let mut`, then for definitive mutations
(assignment, `+=`, `.push()`, `&mut`, `_mut()` methods) within the same
braced scope, classifying each mutation as inside or outside a
`#[cfg(unix)]` region (attribute on item or attribute on statement).

| File:line | Description | Recommended fix | Severity |
|-----------|-------------|-----------------|----------|
| (none) | Scope-aware scan found zero unqualified `let mut` bindings whose mutations are exclusively inside `#[cfg(unix)]` regions. Existing risky cases are already annotated; for example `crates/engine/src/local_copy/executor/file/copy/transfer/finalize.rs:103` already carries `#[allow(unused_mut)] // REASON: mutated on unix for with_fd()`. | Maintain current discipline. When introducing a new `let mut` whose only mutation lives behind `#[cfg(unix)]` or `#[cfg(all(unix, ...))]`, either: (a) move the binding inside the gated block, or (b) add `#[allow(unused_mut)]` with a one-line reason comment. | LOW |

## 3. Rustdoc links on re-exports

Module-level rustdoc (`//!`) lines containing intra-doc links of the
form `` [`TypeName`] `` where the type is re-exported from a sub-module
via `pub use sub::TypeName`. Modern rustdoc generally resolves these,
but the project has historically tripped on cases where the link goes
unresolved silently when a crate does not deny broken intra-doc links.

The scan reports the candidates so the owner can verify that the link
resolves in both `cargo doc` and the docs.rs build. Crates that already
have `#![deny(rustdoc::broken_intra_doc_links)]` enforce this at compile
time; the remaining crates do not, so a broken link there is silent.

### Crates without `#![deny(rustdoc::broken_intra_doc_links)]`

These crates are the highest-risk targets because a broken link would
not fail CI today:

- `crates/logging/src/lib.rs`
- `crates/matching/src/lib.rs`
- `crates/signature/src/lib.rs`
- `crates/test-support/src/lib.rs`

Recommended top-level fix: add
`#![deny(rustdoc::broken_intra_doc_links)]` to each of these crates'
`lib.rs` so the build catches future regressions. This single change is
preferable to chasing individual links.

### Module-level rustdoc links to re-exported items

| File:line | Description | Recommended fix | Severity |
|-----------|-------------|-----------------|----------|
| crates/logging/src/lib.rs:20 | `[`VerbosityConfig`]`, `[`InfoLevels`]`, `[`DebugLevels`]` in `//!` block; types re-exported via `pub use` (see `lib.rs`). Crate lacks `#![deny(rustdoc::broken_intra_doc_links)]`. | Replace with backtick-only `` `VerbosityConfig` `` or qualify as `[`crate::VerbosityConfig`]`. Better: add the deny attribute. | HIGH |
| crates/matching/src/lib.rs:6 | `[`DeltaGenerator`]` in `//!`; re-exported at `lib.rs:34`. | Either qualify as `[`crate::DeltaGenerator`]` or add the deny attribute and let rustdoc verify. | HIGH |
| crates/matching/src/lib.rs:7 | `[`DeltaSignatureIndex`]` in `//!`; re-exported at `lib.rs:35`. | Same as above. | HIGH |
| crates/matching/src/lib.rs:8 | `[`DeltaScript`]`, `[`DeltaToken`]` in `//!`; re-exported at `lib.rs:39`. | Same as above. | HIGH |
| crates/matching/src/lib.rs:9 | `[`FuzzyMatcher`]` in `//!`; re-exported at `lib.rs:30`. | Same as above. | HIGH |
| crates/signature/src/lib.rs:15 | `[`SignatureLayout`]` in `//!`; re-exported via `pub use`. Crate lacks the deny attribute. | Add the deny attribute to surface any breakage. | MED |
| crates/signature/src/lib.rs:17 | `[`FileSignature`]` in `//!`; re-exported via `pub use`. | Same as above. | MED |
| crates/rsync_io/src/lib.rs:16,24,43 | `[`NegotiatedStream`]`, `[`SessionHandshake`]`, `[`TryMapInnerError`]` in `//!`; all re-exported. Crate already has the deny attribute, so links resolve today; flag is informational so anyone moving the type behind a sub-module knows the link will break unless qualified. | No action required while the deny attribute stays in place; just keep the deny attribute. | LOW |
| crates/logging-sink/src/lib.rs:16,21 | `[`MessageSink`]`, `[`LineMode`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/metadata/src/lib.rs:27,41 | `[`MetadataError`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/engine/src/lib.rs:20,22,23,24,33,35,37,49,55,62,67,68,69,103 | 14 distinct `[`TypeName`]` links in `//!`, all re-exported. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/batch/src/lib.rs:33,45,144,152 | `[`BatchHeader`]`, `[`BatchStats`]`, `[`BatchError`]`, `[`BatchReader`]`, `[`BatchWriter`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/daemon/src/lib.rs:23,24 | `[`DaemonConfig`]`, `[`DaemonConfigBuilder`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/checksums/src/lib.rs:35,340,356 | `[`RollingChecksum`]`, `[`RollingSliceError`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/protocol/src/lib.rs:13,26,48 | `[`ProtocolVersion`]`, `[`SUPPORTED_PROTOCOLS`]`, `[`MplexReader`]`, `[`MplexWriter`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/transfer/src/lib.rs:28,68,72,81 | `[`GeneratorContext`]`, `[`ReceiverContext`]`, `[`ServerRole`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/flist/src/lib.rs:16,18,22,28,34,38 | `[`FileListBuilder`]`, `[`FileListWalker`]`, `[`FileListEntry`]`, `[`FileListError`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |
| crates/filters/src/lib.rs:36,38,40,70,97 | `[`FilterRule`]`, `[`FilterSet`]`, `[`FilterError`]` in `//!`. Crate has deny attribute. | Informational; keep the deny attribute. | LOW |

## Summary

| Hazard class | Real hits | Highest severity |
|--------------|-----------|------------------|
| 1. Under-gated test modules | 7 | HIGH |
| 2. `let mut` mutated only under `#[cfg(unix)]` | 0 | - |
| 3. Rustdoc links on re-exports (silent risk crates) | 7 (in logging, matching, signature) | HIGH |

The most cost-effective single follow-up is adding
`#![deny(rustdoc::broken_intra_doc_links)]` to the four crates that
currently lack it. That converts class 3 from a silent hazard into a
build-time error and removes the need for manual auditing on every PR.

The class-1 fixes are mechanical: gate seven `#[path = ...]` lines in
`crates/cli/src/frontend/tests/mod.rs` with `#[cfg(unix)]` next to the
existing `error_recovery.rs`/`daemon.rs` gates.
