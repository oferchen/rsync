# Workspace rustdoc coverage audit

Audit of `///` rustdoc presence on public items across the `crates/` workspace.
Public items counted: `pub fn`, `pub struct`, `pub enum`, `pub trait`, `pub type`,
`pub const`, `pub static`, `pub mod`, `pub union`, `pub macro`.

Excluded from the count:

- `pub use` re-exports (source item carries the doc).
- `pub(crate)` / `pub(super)` / `pub(in path)` (not public API).
- Items inside `macro_rules!` bodies (doc supplied by macro caller).
- Files under any `tests/` directory or named `tests.rs`.

Two coverage views are reported:

- **Lenient** matches rustc's `missing_docs` lint: a `pub mod NAME;` declaration
  counts as documented if the module file (`NAME.rs` or `NAME/mod.rs`) opens with
  a `//!` inner doc.
- **Strict** requires a `///` block on the `pub mod NAME;` declaration itself,
  even when the module file has a `//!` inner doc. Useful for documentation
  consistency rather than rustc satisfaction.

## Workspace summary

- **Lenient coverage:** 99.99% (1 missing of 7190 public items).
- **Strict coverage:** 98.19% (130 missing of 7190 public items).

| Crate | Total pub items | Missing (lenient) | Coverage (lenient) | Missing (strict) | Coverage (strict) |
|---|---:|---:|---:|---:|---:|
| `protocol` | 1290 | 1 | 99.92% | 8 | 99.38% |
| `engine` | 1258 | 0 | 100.00% | 21 | 98.33% |
| `core` | 807 | 0 | 100.00% | 6 | 99.26% |
| `fast_io` | 805 | 0 | 100.00% | 41 | 94.91% |
| `transfer` | 658 | 0 | 100.00% | 7 | 98.94% |
| `rsync_io` | 421 | 0 | 100.00% | 5 | 98.81% |
| `checksums` | 273 | 0 | 100.00% | 7 | 97.44% |
| `metadata` | 255 | 0 | 100.00% | 5 | 98.04% |
| `compress` | 215 | 0 | 100.00% | 4 | 98.14% |
| `daemon` | 199 | 0 | 100.00% | 4 | 97.99% |
| `branding` | 185 | 0 | 100.00% | 3 | 98.38% |
| `filters` | 110 | 0 | 100.00% | 0 | 100.00% |
| `cli` | 107 | 0 | 100.00% | 7 | 93.46% |
| `matching` | 97 | 0 | 100.00% | 1 | 98.97% |
| `flist` | 85 | 0 | 100.00% | 1 | 98.82% |
| `signature` | 76 | 0 | 100.00% | 0 | 100.00% |
| `batch` | 65 | 0 | 100.00% | 0 | 100.00% |
| `logging-sink` | 62 | 0 | 100.00% | 0 | 100.00% |
| `bandwidth` | 60 | 0 | 100.00% | 0 | 100.00% |
| `platform` | 56 | 0 | 100.00% | 7 | 87.50% |
| `apple-fs` | 45 | 0 | 100.00% | 2 | 95.56% |
| `logging` | 35 | 0 | 100.00% | 1 | 97.14% |
| `embedding` | 21 | 0 | 100.00% | 0 | 100.00% |
| `windows-gnu-eh` | 4 | 0 | 100.00% | 0 | 100.00% |
| `test-support` | 1 | 0 | 100.00% | 0 | 100.00% |

## True missing items (lenient view)

Items where rustc itself sees no documentation. These are the only items that
would fail a workspace-wide `#![deny(missing_docs)]` pass.

### `protocol` (99.92%)

- `src/flist/flags.rs:122` - `pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;`

## Strict view: per-crate detail for crates below 95%

In the strict view, the only items counted as missing beyond the lenient list
are `pub mod NAME;` declarations that delegate their docs to the module file's
`//!` block. Listing the worst-offending files per sub-95% crate below.

### `platform` (87.50% strict)

| File | Missing | Total in file | File coverage |
|---|---:|---:|---:|
| `src/lib.rs` | 7 | 8 | 12.5% |

### `cli` (93.46% strict)

| File | Missing | Total in file | File coverage |
|---|---:|---:|---:|
| `src/frontend/mod.rs` | 7 | 9 | 22.2% |

### `fast_io` (94.91% strict)

| File | Missing | Total in file | File coverage |
|---|---:|---:|---:|
| `src/lib.rs` | 18 | 25 | 28.0% |
| `src/io_uring/mod.rs` | 10 | 18 | 44.4% |
| `src/io_uring_stub/mod.rs` | 9 | 10 | 10.0% |
| `src/iocp/mod.rs` | 4 | 4 | 0.0% |

## Strict gap composition

How much of the strict gap is just `pub mod NAME;` declarations that already
rely on `//!` module-file docs.

| Crate | Strict missing | Of which `pub mod NAME;` declarations |
|---|---:|---:|
| `fast_io` | 41 | 41 |
| `engine` | 21 | 21 |
| `protocol` | 8 | 7 |
| `checksums` | 7 | 7 |
| `cli` | 7 | 7 |
| `platform` | 7 | 7 |
| `transfer` | 7 | 7 |
| `core` | 6 | 6 |
| `metadata` | 5 | 5 |
| `rsync_io` | 5 | 5 |
| `compress` | 4 | 4 |
| `daemon` | 4 | 4 |
| `branding` | 3 | 3 |
| `apple-fs` | 2 | 2 |
| `flist` | 1 | 1 |
| `logging` | 1 | 1 |
| `matching` | 1 | 1 |

## Priority recommendations

Priority is weighted by (a) public-API exposure (crates exported from the
workspace root and consumed by the `cli` / `daemon` / `core` orchestration
layer) and (b) recent maintenance activity in `git log`. The available git
history on this branch is shallow, so the activity signal is augmented with
per-crate file count and LoC as proxies for change surface.

### P0 - add the one truly missing doc

- `crates/protocol/src/flist/flags.rs:122` - `pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;`.
  Sits next to a documented `XMIT_HLINK_FIRST` that shares the same bit but
  carries the doc; the second const reuses the bit for the protocol 31+
  end-of-list marker and needs its own one-line `///`.

### P1 - add `///` to `pub mod NAME;` declarations in high-exposure crates

Even though rustc accepts `//!` inner docs, a one-line `///` summary at the
declaration site improves IDE hover, `cargo doc` navigation, and grep-ability.
Prioritize crates that form the public surface of the workspace and that have
a high `pub mod` declaration count in the strict-gap table above:

1. `fast_io` - large public surface for the I/O-uring / IOCP fast-path; lib.rs
   `pub mod` block is the entry point most callers see first.
2. `engine` - workspace's largest crate, public modules are the orchestration
   contract for `core`.
3. `protocol` - wire-format crate; the public modules form the protocol API.
4. `core` - thin orchestration facade; every `pub mod` here is on the hot path
   for downstream embedders.
5. `transfer`, `rsync_io`, `cli`, `checksums` - high public-API exposure but
   small absolute gap; cheap wins.

### P2 - small leaf crates

`platform`, `apple-fs`, `flist`, `matching`, `metadata`, `compress`,
`branding`, `logging`, `daemon` only have a handful of `pub mod` declarations
each. Best handled as a single mechanical pass once the strict policy is
agreed.

### Out of scope for this audit

- No code changes were made; the audit is read-only.
- The audit does not score rustdoc *quality* (length, examples, intra-doc
  links). It only checks presence.
- Macro-generated items, items inside `#[cfg(...)]` blocks that the audit
  cannot evaluate (e.g. platform-only types behind feature gates not
  enabled at scan time) are counted by source presence, not by what rustc
  would compile on a given target.

## Methodology

Implemented as a Python pass over every `crates/*/src/**/*.rs`. For each line
matching `pub <item-keyword>` (and not excluded by the rules above), the audit
walks backward through blank lines, line comments, and one-or-more `#[...]`
attribute blocks (with multi-line bracket balancing) and looks for an
immediately-preceding `///` or `//!`. A nearby `#[doc(hidden)]` short-circuits
the check. For `pub mod NAME;` the lenient pass additionally peeks at the
module file and accepts a top-of-file `//!` as the doc.

