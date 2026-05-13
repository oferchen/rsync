# Windows Target Evaluation (task #1636)

Decide whether to keep `x86_64-pc-windows-gnu` alongside `x86_64-pc-windows-msvc`, or drop the GNU target outright. This document supersedes the higher-level `docs/design/windows-gnu-vs-msvc-evaluation.md` by pinning every claim to a concrete file:line.

Companion design note (audience: maintainers tracking the v0.7.x deprecation arc): `docs/design/windows-gnu-vs-msvc-evaluation.md`.

## 1. The shim

### 1.1 Location and contract

- Crate: `crates/windows-gnu-eh/` (`Cargo.toml` + `src/lib.rs`, 230 lines).
- Manifest contract: `crates/windows-gnu-eh/Cargo.toml:8` describes the crate as "Compatibility shims for x86_64-pc-windows-gnu DWARF unwinding - only needed for GNU cross-compilation, not MSVC".
- Module gating: `crates/windows-gnu-eh/src/lib.rs:61` gates the active code on `#[cfg(all(target_os = "windows", target_env = "gnu"))]`; `crates/windows-gnu-eh/src/lib.rs:204` provides a `const fn force_link()` no-op for every other target.

### 1.2 Symbols it provides

Two `#[no_mangle]` `extern "C"` functions that the Rust startup object `rsbegin.o` references when DWARF unwind data is present:

- `___register_frame_info(eh_frame, object)` - `crates/windows-gnu-eh/src/lib.rs:181`.
- `___deregister_frame_info(eh_frame)` - `crates/windows-gnu-eh/src/lib.rs:191`.

Both forward at runtime to the modern, non-leading-underscore `__register_frame_info` / `__deregister_frame_info` symbols (`crates/windows-gnu-eh/src/lib.rs:84-85`). Resolution is lazy via `GetModuleHandleA` + `LoadLibraryA` + `GetProcAddress` (`crates/windows-gnu-eh/src/lib.rs:87-92`), cached in `AtomicUsize` slots (`crates/windows-gnu-eh/src/lib.rs:74-75`), and silently no-ops if the library is absent (`crates/windows-gnu-eh/src/lib.rs:182-186`, `:192-195`).

Library probe order (`crates/windows-gnu-eh/src/lib.rs:77-82`):

1. `libgcc_s_seh-1.dll`
2. `libgcc_s_sjlj-1.dll`
3. `libgcc_s_dw2-1.dll`
4. `libunwind.dll`

The crate uses `unsafe` for FFI but is wholly self-contained: no dependencies except `core` and `kernel32`.

### 1.3 What it overrides

Nothing. The shim does not replace SEH/DWARF unwinding primitives; it only papers over a missing legacy libgcc entry point that the Zig-provided Windows GNU toolchain omits. The crate's own rustdoc (`crates/windows-gnu-eh/src/lib.rs:8-19`) is explicit: this is a link-time stub that prevents `rsbegin.o` from failing to resolve `___register_frame_info` / `___deregister_frame_info` when DWARF unwind data is generated. There is no exception-handling logic inside the shim; the symbols only matter when DWARF unwinding is active and a runtime library exists to register frames against.

### 1.4 How it is pulled in

Three call sites, all conditionally compiled:

- Workspace member: `Cargo.toml:157` lists `"crates/windows-gnu-eh"` under `[workspace.members]`.
- Conditional dependency: `Cargo.toml:131-133` declares a `[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]` table that pulls in `windows-gnu-eh = { path = "crates/windows-gnu-eh" }`.
- Binary linkage anchor: `src/bin/oc-rsync.rs:16-17` calls `windows_gnu_eh::force_link()` behind `#[cfg(all(target_os = "windows", target_env = "gnu"))]`. The call has no effect; it exists solely to keep the crate's object file (and therefore its `#[no_mangle]` symbols) reachable from the final binary.

There is no `build.rs`, no proc-macro, no conditional feature flag. The dependency is purely a Cargo `target.cfg` predicate.

## 2. GNU-specific divergences across the codebase

A workspace-wide grep for `target_env = "gnu"` (excluding `.claude/worktrees/` and `target/`) returns exactly the call sites listed above:

```
crates/windows-gnu-eh/src/lib.rs:28   (doc comment)
crates/windows-gnu-eh/src/lib.rs:58   (doc comment)
crates/windows-gnu-eh/src/lib.rs:61   active cfg
crates/windows-gnu-eh/src/lib.rs:204  inactive-side cfg
crates/windows-gnu-eh/src/lib.rs:211  pub-use cfg
crates/windows-gnu-eh/src/lib.rs:214  pub-use cfg
src/bin/oc-rsync.rs:16                force_link call
Cargo.toml:132                        target.cfg dependency
```

The remaining `target_env` hits in the workspace are all `musl` gates in `crates/flist/src/batched_stat/` (statx vs libc fallback) and `crates/core/src/version/report/renderer.rs:9-11` (linkage display string). No other crate carries a `cfg(target_env = "gnu")` branch. There is no `cfg(windows_gnu)` custom cfg key.

Other Windows-GNU touch points outside the shim:

- `rust-toolchain.toml:14-15` lists `x86_64-pc-windows-gnu` and `i686-pc-windows-gnu` in `targets`, requiring `rustup` to install those toolchain components for every contributor.
- `.cargo/config.toml:8-9` sets `rustflags = ["-C", "panic=abort"]` for `i686-pc-windows-gnu` (the 32-bit GNU target uses panic=abort to sidestep the 32-bit DWARF unwinder entirely; this is a workaround predating the shim).
- `xtask/src/commands/release/upload.rs:250-251`, `:471`, `:548` reference `x86_64-pc-windows-gnu` / `i686-pc-windows-gnu` paths inside the `cross_compile_target` helper and its tests. These are vestigial: `release-cross.yml` only ever produces MSVC artifacts.

## 3. CI matrix

`.github/workflows/ci.yml` runs five distinct Windows-flavoured jobs:

| Job | Toolchain | Runner | Lines |
|-----|-----------|--------|-------|
| `windows-test` | MSVC, matrix `[stable, beta, nightly]` | `windows-latest` | `.github/workflows/ci.yml:169-216` |
| `windows-iocp` | MSVC, default toolchain | `windows-latest` | `.github/workflows/ci.yml:228-276` |
| `windows-acl-xattr` | MSVC, default toolchain | `windows-latest` | `.github/workflows/ci.yml:286-330` |
| `windows-gnu-cross-check` | GNU, stable, on Linux | `ubuntu-latest` | `.github/workflows/ci.yml:335-359` |

The GNU job is the only one that runs on `ubuntu-latest`. It performs `sudo apt-get install -y mingw-w64` (`.github/workflows/ci.yml:350-351`), installs the `x86_64-pc-windows-gnu` target (`.github/workflows/ci.yml:348`), and runs `cargo check --locked --workspace --target x86_64-pc-windows-gnu` (`.github/workflows/ci.yml:359`). No tests run on GNU; only a type-check.

Release packaging (`.github/workflows/release-cross.yml`) builds and uploads MSVC artifacts only:

- `targets: x86_64-pc-windows-msvc` (`.github/workflows/release-cross.yml:616`).
- Tarball: `cargo xtask package --tarball --tarball-target x86_64-pc-windows-msvc --profile dist` (`.github/workflows/release-cross.yml:640`).
- Zip archive sourced from `target\x86_64-pc-windows-msvc\dist` (`.github/workflows/release-cross.yml:670`).

There is no `x86_64-pc-windows-gnu` job anywhere in `release-cross.yml`.

### 3.1 Historical flakiness

Across the last 100 `ci.yml` runs on `master` (queried via `gh run list`), three runs failed. The failing jobs were:

- Run 25815227124: `Windows (stable)` (MSVC).
- Run 25772073899: `Feature flag combinations / {no-default-features,tracing,compression,async}`.
- Run 25520139001: broad-platform infrastructure outage hitting `macOS`, `Windows`, `Linux musl`, and `nextest` across every toolchain.

`Windows GNU cross-check` does not appear in any failure list. Empirically, it is the most stable Windows job because it only invokes `cargo check`; it has no test runtime, no Windows runner quirks, and no platform-specific filesystem behaviour to flake on.

## 4. User-facing impact of dropping GNU

### 4.1 Release artifacts

- The Homebrew formula (`Formula/oc-rsync.rb:7-17`) ships only macOS Intel and Apple Silicon binaries.
- The release workflow ships Windows binaries only for MSVC (`release-cross.yml:640`, `:670`).
- No `x86_64-pc-windows-gnu` tarball or zip has ever been published from `release-cross.yml`. The GNU target is build-checked, not shipped.

### 4.2 Lost user segments

If GNU is dropped, the following hypothetical consumers lose a supported build path:

- Contributors cross-compiling from Linux without `xwin`/MSVC. They can still build `cargo-zigbuild --target x86_64-pc-windows-msvc` (Zig supports MSVC-style import libraries), but the GNU path is the more common Linux-side recipe.
- MSYS2 / MinGW packagers wanting to ship `oc-rsync` alongside other `mingw-w64` binaries. There is no evidence in the repo that any such packager exists (`Formula/` and `packaging/` carry no MSYS2 manifest).

Neither segment has filed an issue or PR. `gh issue list --search "windows gnu"` returns no open or closed issues other than #1636 itself, which is the PR that introduced the shim.

### 4.3 Upstream rsync posture

Upstream rsync (`target/interop/upstream-src/rsync-3.4.1/`) is a C codebase and historically ships on Windows only through Cygwin or MSYS2 (GNU toolchain). The interop fidelity bar (`target/interop/upstream-src/...`) governs wire format and behaviour, not toolchain selection. oc-rsync's Windows targets are an implementation choice unrelated to upstream's build matrix; matching upstream's GNU toolchain choice has zero protocol benefit and no observable interop benefit (oc-rsync's Windows interop runs locally on MSVC builds anyway).

## 5. Recommendation

**Option A: Drop the GNU target entirely.**

Justification, grounded in the survey above:

1. **Zero shipped artifacts.** `release-cross.yml:616` and `:640` only produce MSVC. The GNU `cargo check` (`ci.yml:359`) has never gated a release. Removing it changes nothing a user can download.
2. **Tiny code surface.** GNU support is one 230-line shim crate plus eight `cfg`/path references (Section 2). Nothing else in the workspace branches on GNU.
3. **No user demand.** No open issue, no packaging script, no Homebrew/MSYS2 manifest in this repository requests a GNU binary. The shim's own rustdoc (`crates/windows-gnu-eh/src/lib.rs:48-54`) flags the crate as removable if GNU goes.
4. **Recurring CI cost.** `ci.yml:335-359` adds an `apt-get install mingw-w64` (~200 MB plus network), a separate cargo cache key, and a full workspace `cargo check`. It runs on every PR and burns minutes despite never having caught a regression that the MSVC jobs missed.
5. **Upstream alignment is irrelevant.** Upstream rsync uses Cygwin/MSYS2 because it is C; oc-rsync is Rust, and Rust's first-class Windows target is MSVC. Tracking upstream's Windows toolchain choice yields no protocol or interop benefit (Section 4.3).
6. **The shim is structurally fragile.** It depends on legacy libgcc entry-point naming, a Zig toolchain quirk, and a runtime probe of four candidate DLLs (`crates/windows-gnu-eh/src/lib.rs:77-82`). It works today only because the probe silently no-ops when nothing resolves. A change in Rust's `rsbegin.o` linkage model would surface as a link error on a target nobody downloads.

Options B and C are explicitly rejected:

- **Option B (keep both, harden the shim).** Hardening would require: (i) building and running an actual `cargo test` matrix on `x86_64-pc-windows-gnu` (not just `cargo check`), (ii) packaging and publishing a GNU release artifact, (iii) adding a self-test that exercises the `___register_frame_info` path under DWARF unwinding. None of those are justified by the (empty) user demand.
- **Option C (best-effort GNU, ungated).** Already effectively the status quo - the cross-check is informational and never gates a release. Keeping it costs CI minutes for no signal. If we are not going to gate on it, we should not run it.

## 6. Migration plan (Option A)

Sequence the work as three small PRs so each is independently reviewable and revertible:

### PR 1 - Remove the CI job and toolchain entries (no functional change)

- Delete the `windows-gnu-cross-check` job, `.github/workflows/ci.yml:332-359`.
- Drop `"x86_64-pc-windows-gnu"` and `"i686-pc-windows-gnu"` from `rust-toolchain.toml:14-15`.
- Remove the `[target.i686-pc-windows-gnu]` `panic=abort` rustflag block from `.cargo/config.toml:8-9`.
- Replace `("windows", "x86_64") => Some("x86_64-pc-windows-gnu")` and `("windows", "x86") => Some("i686-pc-windows-gnu")` in `xtask/src/commands/release/upload.rs:250-251` with `x86_64-pc-windows-msvc` / `i686-pc-windows-msvc` (or drop the `x86` arm entirely - it has no release counterpart). Update the two test paths at `xtask/src/commands/release/upload.rs:471` and `:548` to match.

### PR 2 - Remove the shim crate

- Delete `crates/windows-gnu-eh/` (`Cargo.toml` + `src/lib.rs`).
- Remove the `"crates/windows-gnu-eh"` entry from `Cargo.toml:157`.
- Remove the `[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]` block, `Cargo.toml:131-133`.
- Remove the `#[cfg(all(target_os = "windows", target_env = "gnu"))] windows_gnu_eh::force_link();` call, `src/bin/oc-rsync.rs:16-17`.

### PR 3 - Documentation cleanup

- Mark `docs/design/windows-gnu-vs-msvc-evaluation.md` as superseded (or delete; it was a planning doc and the v0.7.x deprecation cycle it proposes is no longer needed).
- Update this document with a "Status: implemented" header pointing at the merged PR numbers.
- Scan `README.md`, `docs/`, and any installation guide for stray references to `x86_64-pc-windows-gnu`; remove them.

### Verification gate

After PR 2 merges, the next CI run on `master` must show:

- No `Windows GNU cross-check` job in `gh run view <id> --json jobs`.
- All three remaining Windows jobs (`Windows (stable|beta|nightly)`, `Windows IOCP`, `Windows ACL/xattr`) green.
- `git grep -nE 'windows[-_]gnu|target_env = "gnu"' -- ':!target/'` returns zero hits.

### Rollback

If a downstream consumer surfaces a real GNU requirement post-merge, `git revert` the three PRs in reverse order. The shim crate is self-contained, the CI job has no dependents, and the toolchain manifest entries are additive.
