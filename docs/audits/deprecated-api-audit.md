# Deprecated-API audit (Rust stdlib + dependency crates)

Tracking issue: #2124. Companion to the `#[must_use]` coverage sweep
(#2123). Scope: detect call sites in this workspace that invoke
`#[deprecated]` items from either the Rust standard library
(toolchain 1.88) or any third-party crate listed in `Cargo.lock`, then
plan staged removal so the workspace can promote to
`#![deny(deprecated)]`.

## 1. Detection method

Searching the rustc source tree directly is impractical: the
`#[deprecated]` attribute is sprinkled across hundreds of files in
`library/` and many flagged items are gated on platform `cfg`.
The reliable signal is the compiler itself.

```sh
rg "#\[deprecated" $(rustup which rustc | xargs dirname)/../lib/rustlib/src/rust/library/
```

is therefore reference-only. Instead, run a clean workspace build with
the pinned toolchain and grep the diagnostic stream:

```sh
cargo build --workspace --all-features 2>&1 | grep -i deprecated
```

Per the project rule "no local cargo runs", invoke this in CI only
and reference captured output rather than re-running locally. The
workspace already enforces `[workspace.lints.clippy] deprecated =
"deny"` in `Cargo.toml`, so any direct call to a `#[deprecated]` item
fails the existing `fmt+clippy` job.

## 2. Current CI sweep

Latest CI clippy run (master, commit 60e83fd96, toolchain 1.88) emits
**zero** `warning: use of deprecated` lines at `--workspace
--all-features --all-targets`. The `deprecated = "deny"` clippy gate
would have escalated any such warning into a build failure, so the
audit here tracks future-proofing rather than fixing existing call
sites.

## 3. Common 2024-era stdlib deprecations to watch for

Verified absent in `crates/**/*.rs` and `tests/**/*.rs` via `rg -n`:

- `std::env::home_dir` - deprecated since 1.29 (wrong on Windows);
  `home`/`dirs` crate replaces it.
- `std::mem::uninitialized` - long deprecated; use `MaybeUninit`. (We
  do use `mem::zeroed()` for POD `libc` structs - not deprecated, but
  flagged for stylistic follow-up.)
- `try!` macro - replaced by `?`.
- `str::trim_left` / `trim_right` - replaced by `trim_start` /
  `trim_end`.
- `std::sync::ATOMIC_*_INIT` constants - use `AtomicUsize::new` etc.
- `std::sync::ONCE_INIT` - use `Once::new`.
- `std::ascii::AsciiExt` - inherent on `u8`/`char`/`str`.
- `std::error::Error::description` - removed surface.
- `std::sync::atomic::spin_loop_hint` - use `std::hint::spin_loop`.

Lazy-init: 89 source files use `std::sync::OnceLock`; no
`lazy_static!` or `once_cell::sync::Lazy` direct dependencies remain.

Regression guard: keep `rg -n
'home_dir|trim_left|trim_right|try!\(|ONCE_INIT|ATOMIC_.*_INIT|spin_loop_hint|AsciiExt'`
in the audit harness.

## 4. Per-dependency status (`Cargo.lock`)

| Crate | Locked | Notes | Status |
|-------|--------|-------|--------|
| `russh` | 0.60.2 | 0.45 ChannelId/Handler APIs gone in 0.50 | resolved by #1851 |
| `clap` | 4.6.1 | clap 3 builder removed | resolved (clap 4 only) |
| `openssl-sys` | 0.9.115 | not a direct dep; via `native-tls` only | clean |
| `chrono` | not direct | `Date<Tz>`, `Local::today` deprecated upstream | clean (we use `time = "0.3"`) |
| `nix` | 0.31.2 | `Error::Sys` gone since 0.27 | clean |
| `rand` / `rand_core` / `getrandom` / `rand_chacha` | 0.8 + 0.9 + 0.10 | `gen_range` -> `random_range` in 0.10 | **P1** unify on 0.9 |
| `rustix` | 0.38.44 + 1.1.4 | transitive 0.38 still pulled in | **P1** patch consumer or wait on upgrade |
| `rsa` | 0.10.0-rc.16 | retires Marvin attack (RUSTSEC-2023-0071) | **P1** pin once `0.10` is final |
| RustCrypto split (`digest`, `sha1`, `sha2`, `cipher`, `block-buffer`, `generic-array`, `signature`, `inout`, `pbkdf2`, `polyval`, `universal-hash`) | 0.10 + 0.11 | upstream is mid-migration | **P2** bump together once 0.11 stabilises |
| `windows-sys` / `windows-targets` / `windows_*_msvc` | 0.59 + 0.60 + 0.61 | Microsoft + community lag | **P2** ecosystem-driven |
| `hashbrown` | 0.14 + 0.15 + 0.17 | `indexmap`/`std` driven | **P2** ecosystem-driven |
| `lazy_static`, `once_cell` | transitive only | no direct workspace dep | **P2** keep clippy gate |
| `md5` (vs `md-5`) | both present | `md5 0.7` is transitive only; checksums use `md-5` | **P2** advisory |

`cargo deny check advisories` (wired via `deny.toml`) reports no
medium-or-higher RUSTSEC entries against the resolved versions on the
audit date.

## 5. Cleanup plan

1. **Per-crate `#![warn(deprecated)]`.** One PR per workspace crate
   that adds the attribute to `lib.rs`. Clippy's
   `deprecated = "deny"` already escalates the warning at CI; the
   crate-level attribute makes intent discoverable inside each
   crate.
2. **Dependency unification PRs.** `chore(deps): unify rand to 0.9`
   bumps `crates/checksums` (0.8 -> 0.9) and `crates/rsync_io`
   (0.10 -> 0.9). `chore(deps): drop rustix 0.38` patches the
   transitive offender. Track `rsa 0.10` final release in a stub
   issue; flip the pin when it ships.
3. **Promote to `deny`.** After one clean release cycle, flip every
   crate's lint to `#![deny(deprecated)]`. CI's clippy `-D warnings`
   already enforces this transitively.
4. **Regression harness.** Add `tools/no_deprecated.sh` that greps the
   build log for `use of deprecated` and fails the audit job if any
   line appears. Pair with the `rg` regex from Section 3 as a
   source-level guard.
5. **RustCrypto 0.11 follow-up.** When `digest 0.11` finalises, bump
   `crates/checksums/Cargo.toml` and `crates/protocol/Cargo.toml`
   together to eliminate the 0.10/0.11 fork.

CI hooks already in place: `[workspace.lints.clippy] deprecated =
"deny"`, `cargo deny check advisories`, `cargo deny check bans`. The
duplicate-version table above is the current allow-list backlog.
