# Workspace dependency audit

Static audit of the workspace dependency graph. The goal is to surface:

1. Per-crate version pins that resolve to multiple semver-incompatible copies
   in `Cargo.lock`.
2. Crates declared in more than one `Cargo.toml` with consistent version
   constraints that should move to `[workspace.dependencies]`.
3. Dependencies declared in a `Cargo.toml` with no matching `use`/`::`
   reference in the corresponding `src/`, `benches/`, `tests/`, or `build.rs`.

Method: parsed every `crates/*/Cargo.toml` and the root + `xtask` manifests
(filtering out workspace path crates), cross-referenced declarations with
`cargo tree --workspace --duplicates`, and grep-scanned each crate's source
tree for the dependency ident (e.g. `md-5` -> `md5`, `windows-sys` ->
`windows_sys`).

## Version duplicates

### Inside the workspace's own manifests

None. Every dependency declared in two or more workspace crates points at the
same version constraint. The 19-entry `[workspace.dependencies]` table covers
all multi-crate deps that are already consolidated and there are no remaining
pin-vs-pin mismatches across our `Cargo.toml` files.

### Inside `Cargo.lock` (corroborated with `cargo tree --workspace --duplicates`)

`cargo tree --workspace --duplicates` reports the following crates resolved at
two or more versions in the build graph. None of these are caused by drift
inside our manifests; all are pulled in transitively by upstream crates.

| Crate | Versions in lockfile | Why it duplicates |
|---|---|---|
| `chacha20` | 0.9.1, 0.10.0 | 0.9 from `chacha20poly1305 0.10` (dev-dep of `rsync_io`); 0.10 from `rand_core 0.10`. |
| `getrandom` | 0.2.17, 0.3.4, 0.4.2 | 0.2 via `rand_core 0.6`; 0.3 via `rand_core 0.9` (proptest dev-deps); 0.4 via `rand_core 0.10` and `tempfile 3.27`. |
| `hashbrown` | 0.14.5, 0.17.0 | 0.14 via `dashmap 6.1` (dev-dep of `engine`); 0.17 via `indexmap 2.14` -> `toml_edit 0.25` (xtask). |
| `rand` | 0.9.4, 0.10.1 | 0.9 pulled by `proptest 1.11` dev-deps; 0.10 is our workspace pin. |
| `rand_core` | 0.6.4, 0.9.5, 0.10.1 | Mirrors the `rand` situation across older crates in the crypto graph. |
| `semver` | 1.0.28 (x2) | `cargo_metadata 0.18` (xtask) and `rustc_version 0.4` (build-deps). Same version, two separate copies because of build-script dep graph isolation. |
| `thiserror` / `thiserror-impl` | 1.0.69, 2.0.18 | We pin 2.0 workspace-wide; `cargo_metadata 0.18` (xtask) still pulls 1.0. |
| `libc` | 0.2.186 (x2) | Same version, target-graph vs build-graph copies. Not actionable. |

Actionable items in this list:

- `cargo_metadata 0.18` (xtask) is the sole reason 1.x `thiserror`, 1.x
  `semver`, and 0.2.17 `getrandom` are still in the lockfile. Upgrading
  xtask to `cargo_metadata` 0.19+ would drop one `thiserror` major, one
  `getrandom` minor, and shrink build time.
- `proptest 1.11` brings rand 0.9 + rand_core 0.9 + getrandom 0.3. Until
  upstream proptest releases against rand 0.10 there is nothing to do.
- `chacha20poly1305 0.10` is a dev-dep of `rsync_io`; bumping to 0.11
  (when published) collapses chacha20 0.9 -> 0.10.

## Consolidation candidates

Dependencies declared in two or more crates with a consistent version that are
not yet entries in `[workspace.dependencies]`. Moving them into the workspace
table prevents future drift and shortens per-crate manifests.

| Dep | Current pin | Crates | Recommended workspace entry |
|---|---|---|---|
| `assert_cmd` | 2.0 | root bin (dev), `cli` (dev) | `assert_cmd = "2.0"` |
| `globset` | 0.4 | `engine`, `filters` | `globset = "0.4"` |
| `lz4_flex` | 0.13 | `compress`, `protocol` (both optional, both gated on `lz4`) | `lz4_flex = { version = "0.13", default-features = false }`; per-crate `features` list. |
| `md-5` | 0.10 | `checksums` (unix + non-unix targets, different feature flags), `protocol` | `md-5 = { version = "0.10", default-features = false }`; per-target `features` list. |
| `nix` | 0.31 | `apple-fs`, `core`, `platform` | `nix = { version = "0.31", default-features = false }`; per-crate `features` list. |
| `windows` | 0.62 | `daemon`, `metadata`, `platform` | `windows = "0.62"`. |
| `windows-sys` | 0.61 | `engine`, `fast_io` | `windows-sys = "0.61"`; per-crate `features` list. |
| `xattr` | 1.6 | `apple-fs`, `cli`, `daemon` (dev), `engine` (dev), `metadata` (optional) | `xattr = "1.6"`. |
| `zstd` | 0.13 | `compress`, `protocol` (both optional, both gated on `zstd`) | `zstd = "0.13"`. |

Notes:

- `nix` and `windows-sys` need per-crate `features` lists kept locally because
  every crate enables a different subset. The version pin still belongs in the
  workspace.
- The same applies to `md-5` and `lz4_flex` where the two crates enable
  slightly different feature sets.

## Possibly unused dependencies

Manifests that declare a non-optional `[dependencies]` entry with no matching
identifier anywhere under that crate's `src/`, `benches/`, `tests/`, or
`build.rs`. Cross-check before removing - the scan does not see macros,
re-exports, or feature-gated extern usage, so each candidate must be verified
with `cargo +stable check --workspace --all-features` after deletion.

| Crate | Dep | Declaration | Notes |
|---|---|---|---|
| `batch` | `filetime` | `filetime = { workspace = true }` | No `filetime` / `FileTime` reference in `crates/batch/`. |
| `cli` | `tracing-subscriber` | `tracing-subscriber = { workspace = true }` | No `tracing_subscriber` reference in `crates/cli/`. Only `tracing` (the macros crate) is used. |
| `core` | `nix` | `nix = { version = "0.31", default-features = false, features = ["user"] }` (unix target) | No `nix::` reference in `crates/core/`. All Unix-only code uses `std::os::unix` extensions instead. |
| `core` | `rustix` | `rustix = { workspace = true, features = ["process"] }` (unix target) | No `rustix::` reference in `crates/core/`. |
| `engine` | `windows-sys` | `windows-sys = { version = "0.61", features = ["Win32_Storage_FileSystem"] }` (windows target) | No `windows_sys::` reference anywhere in `crates/engine/`. |
| `matching` | `thiserror` | `thiserror = { workspace = true }` | No `thiserror`, `#[derive(Error)]`, or manual `Error` impl in `crates/matching/`. |
| `platform` | `thiserror` | `thiserror = { workspace = true }` | Same situation as `matching`. |

These seven entries are the safest cleanups; removing them shortens compile
graphs on the relevant platforms without risking behavioural change. The
windows-only and unix-only entries in particular are worth deleting because
they extend the dependency cone on a single target and are easy to miss in
review.

## Out of scope

- Transitive deps pulled by external crates are listed for context only - we
  do not control them.
- Optional deps gated behind features were not flagged as unused unless the
  source tree contained no `dep:<name>` feature gate either.
- Per-target dependency tables were correctly classified; `nix` and the
  `cfg(unix)` half of `md-5` are real declarations, not parse artefacts.
