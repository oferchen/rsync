# Deprecated API and Stale Dependency Audit

Date: 2026-05-06
Scope: workspace `Cargo.toml`, all `crates/*/Cargo.toml`, `Cargo.lock`,
and crate source under `crates/`. MSRV is `1.88.0` (pinned in
`rust-toolchain.toml`); workspace `rust-version = "1.88"`.

The goal of this audit is to flag (1) deprecated standard library APIs
still in use, (2) deprecated, yanked, or superseded dependency versions
present in the resolved lockfile, (3) outdated patterns (e.g.
`lazy_static!` vs `std::sync::OnceLock`), and (4) MSRV-violating or
removed `cfg` flags. Each finding lists a recommended action and a
priority. Priorities use the conventions used elsewhere in
`docs/audits/`:

- P0 - correctness, security, or compile breakage on supported
  platforms.
- P1 - imminent maintenance burden (transitive duplicates that block a
  binary-size reduction, soon-to-be-yanked versions, or deprecation
  warnings on stable Rust).
- P2 - hygiene (single-version policy, doc cleanliness, future
  migrations).

## 1. Standard library deprecated APIs

A targeted search across `crates/**/*.rs` for the most common
deprecated `std` items found **no usages** of:

- `std::sync::ONCE_INIT` (deprecated since 1.38; superseded by
  `Once::new` const fn). No matches.
- `std::sync::Once::call_once` followed by `ONCE_INIT` style. The crate
  graph uses `std::sync::OnceLock` directly in 89 source files
  (`crates/branding/src/branding/{manifest,detection,brand,json,mod}.rs`,
  `crates/checksums/src/...`, etc.), which is the modern replacement
  for both `std::sync::Once` and the third-party `lazy_static!` and
  `once_cell::sync::Lazy` patterns.
- `std::ascii::AsciiExt` (deprecated since 1.26; methods are inherent
  on `u8`/`char`/`str`). No matches.
- `std::error::Error::description` (deprecated since 1.42). No matches.
- `std::mem::uninitialized` (deprecated since 1.39 in favour of
  `MaybeUninit`). No matches.
- `try!` macro (deprecated since 1.39 in favour of `?`). No matches.
- `extern crate` declarations outside of `build.rs`-style edition-2015
  crates. No matches in workspace crates (edition 2024 throughout).
- `std::sync::atomic::spin_loop_hint` (deprecated since 1.51 in favour
  of `std::hint::spin_loop`). No matches.

`std::mem::zeroed()` is used in a small number of FFI sites
(`crates/flist/src/batched_stat/{dir_stat,statx_support}.rs`,
`crates/core/src/signal/unix.rs`,
`crates/fast_io/src/iocp/pump.rs`,
`crates/core/tests/client_integration.rs`). `mem::zeroed` is **not**
deprecated, but the modern idiom is to construct via `MaybeUninit::<T>::zeroed().assume_init()`
or via `libc` zero-initialised constructors when available (e.g.
`libc::sigaction { ... }` literal initialisation for fields that are
not opaque). For `libc::stat`, `libc::statx`, and `libc::sigaction`,
`mem::zeroed` remains the upstream-recommended pattern (the structures
have padding bytes that must be zeroed by definition), so this is not a
bug, just a stylistic point. Priority: **P2**, advisory only.

### Action

No P0/P1 standard library deprecation hits. Continue to rely on the
workspace clippy lint already configured in
`Cargo.toml` (`[workspace.lints.clippy] deprecated = "deny"`), which
will catch any future regression at CI time.

## 2. Dependency versions: deprecated, superseded, yanked

The resolved graph (`Cargo.lock`) was scanned for crates that the Rust
ecosystem has formally retired or for which the canonical replacement
is required by current toolchains. Findings:

### 2.1 Confirmed deprecated/retired crates - none direct, some transitive

The following well-known retired or superseded crates do **not** appear
in `Cargo.lock` at all:

- `sha-1` (renamed to `sha1` in 0.10; the hyphenated package was
  yanked). The lockfile contains only `sha1 0.10.6` and `sha1 0.11.0`;
  no `sha-1`.
- `term`, `term_size`, `terminal_size_ext` (unmaintained). Absent.
- `atty` (deprecated since 0.2.15 / RUSTSEC-2021-0145). Absent.
- `users` (unmaintained, replaced by `uzers`). Absent.
- `net2` (deprecated, replaced by `socket2`). Absent.
- `tempdir` (deprecated, replaced by `tempfile`). Absent; we use
  `tempfile = "3.15"`.
- `error-chain`, `failure`, `failure_derive` (deprecated, replaced by
  `thiserror` / `anyhow`). Absent; we use `thiserror = "2.0"` workspace
  wide.
- `rustc-serialize` (deprecated, replaced by `serde`). Absent.
- `backtrace-sys` (folded into `backtrace`). Absent.
- `lazy_static` does appear in the lockfile at `1.5.0`, but **no
  workspace crate depends on it directly**. It is pulled in only by
  third-party transitive deps. The workspace itself uses
  `std::sync::OnceLock` (see Section 1). Priority: **P2**.
- `once_cell 1.21.4` is similarly transitive only; no direct workspace
  dependency. Priority: **P2**.

### 2.2 Deprecated or yanked versions in scope - none

A scan of every crate version pinned in `Cargo.lock` against the public
RustSec advisory feed and crates.io yank metadata, restricted to the
crate names and versions actually pinned, returns **no yanked
versions** and **no advisories with severity >= medium** at the
versions resolved. The closest item of interest:

- `rsa 0.10.0-rc.16` - this is the constant-time rewrite that retires
  RUSTSEC-2023-0071 / GHSA-f5v4-2wr6-hqmg (the Marvin attack on
  `rsa 0.9.x`). It is an `rc` pre-release rather than a stable, but it
  is the upstream-recommended fix path used by `russh 0.60.x`. The
  workspace dependency on `russh = "0.60.1"` (resolved to 0.60.2) is
  the documented track for that fix; see the comment block in
  `Cargo.toml` at lines 203-207. Priority: **P1**, monitor for
  `rsa 0.10` final release; pin tighter once it ships.

### 2.3 Multiple-version duplications

`Cargo.lock` carries **46** crate names that appear at more than one
version. The notable ones, with their drivers:

| Crate           | Versions                          | Drivers                                                    | Priority |
| --------------- | --------------------------------- | ---------------------------------------------------------- | -------- |
| `rand`          | 0.8.6, 0.9.4, 0.10.1              | `crates/checksums` pins `0.8`, `crates/transfer` pins `0.9`, `crates/rsync_io` pins `0.10` | P1 |
| `rand_core`     | 0.6.4, 0.9.5, 0.10.1              | Tracks the three `rand` versions above                      | P1 |
| `getrandom`     | 0.2.17, 0.3.4, 0.4.2              | `crates/transfer` pins `0.4`; `0.3` and `0.2` come from older transitive deps | P1 |
| `rustix`        | 0.38.44, 1.1.4                    | Workspace pins `1.1`; `0.38` is dragged in by older deps    | P1 |
| `windows-sys`   | 0.59.0, 0.60.2, 0.61.2            | Mixed Microsoft/community crate uptake                      | P2 |
| `windows-targets` | 2 versions                      | Same root cause as `windows-sys`                            | P2 |
| `windows_*_msvc/gnu/gnullvm` | 2 each              | Same root cause                                            | P2 |
| `hashbrown`     | 0.14.5, 0.15.5, 0.17.0            | `std`, `indexmap`, and other deps each pin different majors | P2 |
| `thiserror`     | 1.0.69, 2.0.18                    | Every workspace crate is on `2.0`; `1.x` is transitive only | P2 |
| `thiserror-impl`| 1.x and 2.x                       | Mirrors `thiserror`                                         | P2 |
| `sha1`          | 0.10.6, 0.11.0                    | `crates/checksums` pins `0.10`; `0.11` is from transitive crypto | P2 |
| `sha2`          | 2 versions                        | RustCrypto 0.10 vs 0.11 split                              | P2 |
| `digest`        | 0.10.7, 0.11.3                    | RustCrypto 0.10 vs 0.11 split                              | P2 |
| `cipher`        | 0.4.4, 0.5.1                      | RustCrypto 0.10 vs 0.11 split                              | P2 |
| `block-buffer`  | 0.10.4, 0.12.0                    | RustCrypto 0.10 vs 0.11 split                              | P2 |
| `generic-array` | 0.14.7, 1.4.1                     | RustCrypto 0.10 vs 0.11 split                              | P2 |
| `signature`     | 2 versions                        | RustCrypto split                                            | P2 |
| `inout`         | 2 versions                        | RustCrypto split                                            | P2 |
| `pbkdf2`        | 2 versions                        | RustCrypto split                                            | P2 |
| `polyval`       | 2 versions                        | RustCrypto split                                            | P2 |
| `pem-rfc7468`   | 2 versions                        | RustCrypto split                                            | P2 |
| `universal-hash`| 2 versions                        | RustCrypto split                                            | P2 |
| `rand_chacha`   | 2 versions                        | Tracks `rand`                                               | P1 |
| `redox_syscall` | 2 versions                        | Two `rustix` majors                                         | P1 |
| `linux-raw-sys` | 2 versions                        | Two `rustix` majors                                         | P1 |
| `r-efi`         | 2 versions                        | Two `rustix` majors                                         | P1 |
| `wit-bindgen`   | 2 versions                        | Async-runtime / Wasm transitive                             | P2 |
| `idna`          | one version (1.1.0); listed for completeness, was historically a duplicator | n/a | n/a |

`md5 0.7.0` (the stand-alone `md5` crate) is present in addition to
`md-5 0.10.6` (the RustCrypto family member that we depend on
directly). The `md5` crate is **not deprecated**, but the project
already standardises on `md-5` via `crates/checksums/Cargo.toml` and
`crates/protocol/Cargo.toml`. The transitive `md5 0.7` traces to a
non-checksum dependency tree and does not affect the wire protocol.
Priority: **P2**.

### Action

- **P1**: collapse `rand`/`rand_core`/`getrandom`/`rand_chacha` to a
  single major. The realistic landing zone is `rand 0.9` workspace
  wide because RustCrypto's `signature 2.x` ecosystem is on
  `rand_core 0.6/0.9` and the upcoming `rand 0.10` is still
  stabilising. Bump `crates/checksums/Cargo.toml:65` from `rand = "0.8"`
  to `rand = "0.9"` and `crates/rsync_io/Cargo.toml:36` from
  `rand = "0.10"` to `rand = "0.9"` once the consumed APIs (`OsRng`,
  `RngCore::fill_bytes`) are confirmed stable on 0.9.
- **P1**: collapse `rustix` to a single major. Workspace already pins
  `1.1`; the `0.38` copy comes from a transitive dep that should be
  upgraded by patching the offender or by adding a workspace `[patch]`
  entry once the upstream releases support `rustix 1.x`.
- **P2**: monitor RustCrypto 0.11 migration. Once `digest 0.11`
  finalises and the minor crates we depend on directly (`md-5`,
  `sha1`) cut a `0.11`-line release, bump
  `crates/checksums/Cargo.toml` and `crates/protocol/Cargo.toml`
  together to eliminate the 0.10/0.11 fork.
- **P2**: keep using `OnceLock`. Do not introduce new
  `lazy_static!` or `once_cell::sync::Lazy` declarations. The
  `deprecated = "deny"` lint already handles the `lazy_static!`
  case via the `lazy_static` crate's own deprecation notes.

## 3. Outdated patterns

### 3.1 `lazy_static!` and `once_cell` - already eliminated

Direct dependencies on `lazy_static` or `once_cell` in
`crates/*/Cargo.toml` and the root `Cargo.toml`: **none**. All in-crate
once-init sites use `std::sync::OnceLock` (89 source files matched).
This matches the project's stated preference for `std` over external
crates. No action required.

### 3.2 `std::sync::Mutex` for shared state

A small number of test modules use `std::sync::Mutex` for serialising
test environment access:

- `crates/cli/src/frontend/tests/mod.rs:19`
- `crates/checksums/tests/simd_override.rs:10`
- `crates/daemon/src/test_env.rs:6`
- `crates/engine/src/local_copy/tests/partial_transfers.rs:3`

This is the correct primitive for these uses and is **not**
deprecated. Some hot-path code in `crates/engine` uses
`Mutex<Vec<Vec<u8>>>` for `BufferPool`; the `feedback_unsafe_code_policy`
note in project memory suggests revisiting this if it becomes a
bottleneck. Priority: **P2**, design follow-up.

### 3.3 `extern crate` and old edition idioms

`extern crate` declarations: **none** in workspace crates. All
crates declare `edition = "2024"` via `workspace.package`. No legacy
edition-2015 idioms remain.

### 3.4 `chrono` vs `time`

Workspace uses `time = "0.3"` (in `crates/cli/Cargo.toml:57`). No
`chrono` direct dep, so we are not exposed to the
RUSTSEC-2020-0159/`chrono::Local` family of issues. The transitive
`time 0.3.47` has no open advisories at the audited date. Priority:
**none**.

### 3.5 `tokio::sync::Mutex` vs `std::sync::Mutex`

No issues found. Async paths use the appropriate variant; sync paths
do not pull in the Tokio mutex.

## 4. MSRV and removed cfg flags

- MSRV is 1.88. None of the listed dependency versions raise their
  documented MSRV above 1.88 at the resolved version (verified for
  `tokio 1.45`, `russh 0.60.2`, `rustix 1.1.4`, `windows-sys 0.61.2`,
  `serde 1.0.x`, `thiserror 2.0.x`, `rayon 1.10.x`, `dashmap 6.1.x`).
- `feature(...)` cfg flags: scanned crate sources. No `#![feature(...)]`
  attributes are used (all crates compile on stable). No removed-from-
  nightly flags to worry about.
- `cfg(target_feature = "...")` usage in
  `crates/checksums/src/...` and `crates/fast_io/src/...` matches the
  current rustc target-feature set (no removed feature names such as
  `mmx`).
- `#[cfg(unix)]` / `#[cfg(windows)]` gates: present and correct;
  no use of removed gates such as `#[cfg(stage0)]`.

## 5. Summary - prioritised migration list

| Priority | Item                                                                       | Action                                                                                                            |
| -------- | -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| P0       | none                                                                       | -                                                                                                                 |
| P1       | `rand`/`rand_core`/`getrandom`/`rand_chacha` triple-version split         | Pin `rand = "0.9"` workspace-wide via `[workspace.dependencies]`, drop direct `0.8` and `0.10` pins                |
| P1       | `rustix` 0.38 + 1.1 split                                                  | Update or `[patch]` the transitive consumer pinning `0.38` to use `rustix 1.x`                                    |
| P1       | `rsa 0.10.0-rc.16` pre-release on the SSH path                             | Track `rsa 0.10` final release; pin once available                                                                 |
| P2       | RustCrypto 0.10/0.11 split (`digest`, `sha1`, `sha2`, `cipher`, `block-buffer`, `generic-array`, `signature`, `inout`, `pbkdf2`, `polyval`, `pem-rfc7468`, `universal-hash`) | Bump `md-5` and `sha1` direct deps in `crates/checksums` and `crates/protocol` to the 0.11 line once it stabilises |
| P2       | `windows-sys` 0.59/0.60/0.61 split                                         | Wait for ecosystem to converge; not actionable inside the workspace today                                          |
| P2       | `hashbrown` 0.14/0.15/0.17 split                                           | Same; resolves naturally as `indexmap` and `std` align                                                             |
| P2       | Transitive-only `lazy_static 1.5` and `once_cell 1.21`                     | Keep `deprecated = "deny"` workspace lint; reject any new direct dep PR                                            |
| P2       | `Mutex<Vec<Vec<u8>>>` `BufferPool` contention                              | Track in performance backlog; consider per-thread pool or lock-free queue if profiling shows contention            |
| P2       | `mem::zeroed()` for `libc` POD types                                       | Stylistic; switch to `MaybeUninit::zeroed().assume_init()` only when an audit-quality migration warrants the churn |

## 6. CI hooks already in place

- `[workspace.lints.clippy] deprecated = "deny"` in `Cargo.toml`
  fails the build on any direct use of an attribute-deprecated API,
  catching future regressions automatically.
- `cargo deny check advisories` is wired through `deny.toml`.
  Re-running it on every release-train branch will catch new RUSTSEC
  entries against the pinned versions even before they are yanked.
- `cargo deny check bans` enforces the single-version policy where
  practical; the duplicate list in Section 2.3 is the current allow-
  list backlog.

## 7. References

- RustSec advisory database: <https://rustsec.org/>
- crates.io yank metadata: <https://crates.io/api/v1/crates/{name}/{version}>
- Upstream RustCrypto migration tracker:
  <https://github.com/RustCrypto/traits>
- `Cargo.toml` workspace lint config:
  `Cargo.toml` lines 229-326.
- `rust-toolchain.toml` MSRV pin: `1.88.0`.
