# Workspace Dependency Audit

Audit of the `[workspace.dependencies]` table in the root `Cargo.toml`, plus a
sweep of per-crate `Cargo.toml` files for version drift, inconsistent declaration
patterns, and duplicate transitive deps in `Cargo.lock`.

Scope:

- Root `Cargo.toml` workspace deps section (21 entries).
- All 26 crate `Cargo.toml` files plus `xtask/Cargo.toml`.
- `Cargo.lock` for transitive duplication.

Method (manual; `cargo-udeps` and `cargo-outdated` not installed on the audit
host):

- For each workspace dep, locate every `Cargo.toml` that references it.
- For workspace deps that resolve to a single crate, confirm at least one source
  file imports the crate.
- For common direct deps (declared in multiple crates without the workspace
  table), tally version variants.
- Inspect `Cargo.lock` for crates appearing under more than one version.

## Workspace dependencies status

| Dep | Version | Status | Notes |
|---|---|---|---|
| `mimalloc` | 0.1 | used | Windows global allocator in `src/bin/oc-rsync.rs` (`#[cfg(windows)]`). Unix uses jemalloc. |
| `tikv-jemallocator` | 0.6 | used | Unix global allocator in `src/bin/oc-rsync.rs` (`#[cfg(unix)]`); page return tuned via the `_rjem_malloc_conf` static (dirty/muzzy decay 250 ms) to bound RSS at scale. |
| `rustc-hash` | 2.1 | used | `engine`, `matching`, `protocol`. |
| `jwalk` | 0.8 | used | `engine/src/walk/walkdir_impl.rs`. |
| `exacl` | 0.13 | used | `metadata` (acl feature), `daemon` (dev-dep, unix only). |
| `tokio` | 1.52 | used | `bandwidth`, `core`, `daemon`, `engine`, `rsync_io`, `transfer`. See drift below. |
| `tokio-util` | 0.7 | used | `transfer` (sync), `protocol` (codec - declares its own copy). See drift. |
| `serde` | 1.0 | used | `flist`, `logging`, `protocol`. See drift. |
| `serde_json` | 1.0 | used | `logging`, `protocol`. See drift. |
| `tracing` | 0.1 | used | `cli`, `core`, `daemon`, `engine`, `filters`, `logging`, `matching`, `protocol`, `signature`, `transfer`. |
| `tracing-subscriber` | 0.3 | used | `cli`, `core` (dev), `logging`. |
| `rayon` | 1.10 | used | `cli`, `engine`, `fast_io`, `signature`, `transfer`. See drift. |
| `crossbeam` | 0.8 | **unused** | Umbrella crate. No source file imports `crossbeam::*`. `crossbeam-channel` and `crossbeam-queue` are pulled in directly by `engine` and `transfer`. |
| `crossbeam-channel` | 0.5 | used | `engine`, `transfer` (dev). |
| `dashmap` | 6.1 | used | `daemon` (concurrent-sessions feature), `engine` (dev, unix). |
| `russh` | 0.60.1 | used | `rsync_io` (embedded-ssh feature). |
| `url` | 2 | used | `rsync_io` (embedded-ssh). |
| `rpassword` | 7 | used | `rsync_io` (embedded-ssh). |
| `is-terminal` | 0.4 | used | `rsync_io` (embedded-ssh). |
| `raw-cpuid` | 11 | **unused** | Declared in `crates/rsync_io/Cargo.toml` as `optional = true` but no source file imports `raw_cpuid` and no feature gates `dep:raw-cpuid`. Dead since introduction. |
| `zeroize` | 1 | used | `core/src/client/module_list/auth.rs`. |

## Drift: workspace dep bypassed by per-crate declaration

These crates declare a dep directly that already exists in the workspace table.
Each direct declaration should be replaced with `{ workspace = true, ... }` to
keep versions in one place.

| Dep | Crate(s) bypassing workspace | Per-crate version |
|---|---|---|
| `tokio` | `crates/protocol` | `1.52` (matches workspace) |
| `tokio-util` | `crates/protocol` | `0.7` (matches workspace) |
| `serde` | `crates/branding`, `xtask` | `1` (compatible) |
| `serde_json` | `crates/branding`, `xtask` | `1` (compatible) |
| `rayon` | `crates/checksums`, `crates/flist` | `1.10` (matches workspace) |

These are all version-compatible today, so this is a tidy-up. The
`protocol/tokio = "1.52"` pin is the one most likely to drift; aligning it
with the workspace removes a future hazard.

## Drift: common deps not workspaced

These deps appear in 3+ crate `Cargo.toml` files with their own version pin.
Adding them to `[workspace.dependencies]` would prevent the kind of drift seen
below.

| Dep | Crates | Version variants observed |
|---|---|---|
| `thiserror` | 18 crates | `2.0` everywhere (consistent). |
| `tempfile` | 22 crates | `3.15` everywhere, except `checksums` at `3.14`. |
| `libc` | 10+ crates | `0.2` everywhere. |
| `memchr` | `bandwidth`, `protocol`, `rsync_io` | `2.7` everywhere. |
| `criterion` | 9 crates (dev) | `0.8` everywhere, mixed features. |
| `proptest` | 8 crates (dev) | `1.4` mostly, but `bandwidth` and `engine` at `1.8`. |
| `rand` | `checksums` `0.8`, `transfer` `0.9`, `rsync_io` `0.10` | three coexisting majors (dev-deps only). |
| `crossbeam-queue` | `engine`, `transfer` | `0.3` everywhere. |
| `flate2` | `protocol`, `xtask` | `1.0` / `1.0.28`. |
| `filetime` | `core`, `engine` (opt), top-level (dev), `daemon` (dev) | `0.2` everywhere. |
| `socket2` | `core`, `daemon` | `0.6` everywhere. |
| `toml` | `branding`, `xtask` | `1.1` everywhere. |
| `clap` | `cli`, `daemon`, `xtask` | `4.5.x` everywhere. |
| `base64` | `core`, `daemon` | `0.22` everywhere. |
| `rustix` | `core`, `engine` | `1.1` everywhere. |

The `rand` family is the most actionable drift: three different majors are
linked into the dev test binaries. `0.10` is the latest and supersedes the
others.

## Duplicate transitive deps (Cargo.lock)

`Cargo.lock` contains the following duplicate crate names. Most are pulled in
through `russh` (older cryptography stack) or `digest`-family forks. None are
strictly removable from oc-rsync itself; they are listed for visibility.

| Crate | Versions | Brought in by |
|---|---|---|
| `rand` | `0.8.6`, `0.9.4`, `0.10.1` | `checksums` / `transfer` / `rsync_io` dev-deps (workspaceable). |
| `rand_core` | `0.6.4`, `0.9.5`, `0.10.1` | Follows `rand`. |
| `getrandom` | `0.2.17`, `0.3.4`, `0.4.2` | `rand_core 0.6`, `rand_core 0.9`, `transfer` direct. |
| `hashbrown` | `0.14.5`, `0.15.5`, `0.17.0` | Indirect (`indexmap`, `dashmap`, std-internal). |
| `windows-sys` | `0.59.0`, `0.60.2`, `0.61.2` | Multiple transitive deps. |
| `windows-targets` | `0.52.6`, `0.53.5` | Follows `windows-sys`. |
| `windows_*` (per-arch) | `0.52.6`, `0.53.5` | Follows `windows-targets`. |
| `thiserror` / `thiserror-impl` | `1.0.69`, `2.0.18` | Transitive `1.x` from `russh`/crypto stack. Our own crates already use `2.0`. |
| `digest`, `sha1`, `sha2`, `cpufeatures` | 0.10 + 0.11 / 0.2 + 0.3 | `0.11` family pulled in by `russh` chain; our crates pin `0.10`. |
| `aes`, `aes-gcm`, `aead`, `cipher`, `ctr`, `cbc`, `chacha20`, `ghash`, `hmac`, `inout`, `polyval`, `universal-hash`, `block-buffer`, `block-padding`, `crypto-common`, `generic-array`, `signature` (crate), `pbkdf2`, `pem-rfc7468`, `const-oid`, `rand_chacha`, `r-efi`, `wit-bindgen` | two versions each | All pulled in through the `russh` 0.60 dependency tree. |

The `russh` chain accounts for the bulk of the duplication. Cleaning it up
requires russh upstream to bump its own dep set; there is nothing to do in this
repo today.

## Outdated check

`cargo-outdated` is not installed in the audit environment. A spot check of
workspace dep majors against crates.io shows the workspace is current: `rayon`
`1.10`, `dashmap` `6.1`, `rustc-hash` `2.1`, `tokio` `1.5x`, `tokio-util`
`0.7`, `russh` `0.60.1`, `exacl` `0.13`, `jwalk` `0.8`, `tracing` `0.1.x`,
`tracing-subscriber` `0.3.x`, `zeroize` `1.x`, `serde`/`serde_json` `1.x`.
Nothing in the workspace table is a major behind.

## Recommendations

### Applied in this PR (safe removals)

1. Remove `crossbeam = "0.8"` from `[workspace.dependencies]`. The umbrella
   crate is never imported; only `crossbeam-channel` (already workspaced) and
   `crossbeam-queue` are used.
2. Remove `raw-cpuid = "11"` from `[workspace.dependencies]` and drop the
   matching `raw-cpuid = { workspace = true, optional = true }` entry from
   `crates/rsync_io/Cargo.toml`. No source file references it and no feature
   activates it.

### Follow-ups (deferred to focused PRs)

3. Add `thiserror = "2.0"`, `tempfile = "3.15"`, `libc = "0.2"`, `memchr =
   "2.7"`, `criterion = "0.8"`, `proptest = "1.8"`, `rand = "0.10"`,
   `crossbeam-queue = "0.3"`, `flate2 = "1.0"`, `filetime = "0.2"`, `socket2 =
   "0.6"`, `toml = "1.1"`, `clap = "4.5"`, `base64 = "0.22"`, `rustix = "1.1"`
   to `[workspace.dependencies]` and rewrite per-crate entries as `{ workspace
   = true, ... }`. Unblocks single-point version updates and removes the
   `rand 0.8/0.9/0.10` and `tempfile 3.14/3.15` and `proptest 1.4/1.8` drift.
4. Convert `tokio`, `tokio-util`, `serde`, `serde_json`, `rayon` direct
   declarations in `crates/protocol`, `crates/branding`, `crates/checksums`,
   `crates/flist`, and `xtask` to `{ workspace = true, ... }`.
5. After (3), revisit the `Cargo.lock` duplication table; the
   `digest 0.10`/`0.11` split and `cpufeatures 0.2`/`0.3` split will collapse
   once `russh` upgrades, and the `getrandom 0.2`/`0.3` axis will narrow once
   `rand` is unified to `0.10`.
