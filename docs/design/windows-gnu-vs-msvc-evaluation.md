# Windows GNU vs MSVC Target Evaluation

Tracking issue: #1636. Decide whether to keep `x86_64-pc-windows-gnu` alongside `x86_64-pc-windows-msvc`, or drop GNU entirely.

## 1. Current state

The repository ships a dedicated GNU compatibility shim and a CI cross-check, but releases exclusively use MSVC.

- Crate: `crates/windows-gnu-eh/` (Cargo.toml + `src/lib.rs`, ~230 lines). Provides `___register_frame_info` / `___deregister_frame_info` no-op shims that lazily forward to libgcc/libunwind on `cfg(all(target_os = "windows", target_env = "gnu"))`. On every other target it collapses to a `force_link()` no-op.
- Workspace wiring: root `Cargo.toml` lists the crate under `[workspace.members]` (line 157) and pulls it as a dependency under `[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]` (lines 132-133).
- Binary call site: `src/bin/oc-rsync.rs:17` calls `windows_gnu_eh::force_link()` behind `#[cfg(all(target_os = "windows", target_env = "gnu"))]` to guarantee linker retains the shim object.
- CI: `.github/workflows/ci.yml` job `windows-gnu-cross-check` (lines 332-359) installs `mingw-w64` on `ubuntu-latest`, adds the `x86_64-pc-windows-gnu` target, and runs `cargo check --locked --workspace --target x86_64-pc-windows-gnu`. Three other Windows jobs (`windows-test`, `windows-iocp`, `windows-acl-xattr`) run on `windows-latest` and exercise MSVC.
- Releases: `.github/workflows/release-cross.yml` builds and packages only `x86_64-pc-windows-msvc` artifacts. No GNU tarball or zip is published.

## 2. Cost of keeping the GNU target

- **Crate maintenance.** `windows-gnu-eh` is unsafe FFI that mirrors libgcc symbol naming. Any Rust change to `rsbegin.o` linkage, or any shift in libunwind/libgcc symbol resolution, requires re-validating the shim.
- **CI footprint.** `windows-gnu-cross-check` adds an `apt-get install mingw-w64` step (network + ~200 MB), a separate cargo cache key, and a full workspace `cargo check` against a target whose artifacts are never released. It also blocks the `lint` -> downstream chain on a check no consumer depends on.
- **Upstream divergence.** Upstream rsync is C, compiled with the platform default toolchain. Carrying a Rust-specific DWARF unwinding shim has no upstream analogue, so reviewing or porting protocol changes requires extra context every time the GNU path surfaces.
- **Cognitive overhead.** Every new Windows feature (IOCP, ACLs, xattrs, ADS) must be mentally checked against the GNU cross-check job in addition to the three MSVC jobs.

## 3. Benefits of keeping GNU

- **Cross-compile from Linux.** `cargo-zigbuild` or `cross` against `x86_64-pc-windows-gnu` lets contributors without a Windows host produce a Windows binary. MSVC cross-compilation requires `xwin` plus a Microsoft EULA acceptance.
- **No MSVC license dependency.** GNU avoids redistributing or accepting Microsoft Build Tools licensing during local development.
- **OpenSSL linkage.** Some downstream packagers historically prefer the GNU toolchain because OpenSSL builds against MinGW use the same `pkg-config` flow as Linux. oc-rsync uses `rustls`/`ring`, so this advantage does not apply to our build.

## 4. Who consumes the GNU build?

- No release artifact for GNU has ever been published; only MSVC tarballs and zips ship from `release-cross.yml`.
- No issue, PR, or downstream packaging script in this repository requests a GNU binary.
- Native Windows users install MSVC binaries from the GitHub Release or the Homebrew formula (which delegates to the MSVC binary). Server operators on Linux use the Linux musl artifact, not a Windows-GNU one.
- Expected real consumer count: zero. The cross-check exists to keep the option open, not because anyone needs it.

A short user survey on the tracking issue (#1636) is the gating step before deprecation. Question to post: *"Are you currently consuming, packaging, or planning to consume the `x86_64-pc-windows-gnu` build of oc-rsync? If yes, please describe your toolchain (cargo-zigbuild, cross, MSYS2, other) and why MSVC is not viable."*

## 5. Recommendation

**Deprecate GNU in v0.7.x with a one-release notice; remove in v0.8.x.** Continue running the GNU cross-check during the deprecation window so anyone who objects has a working build to point at.

### v0.7.x deprecation actions

- Add a `## Deprecations` section to the v0.7.x release notes announcing GNU removal in v0.8.x and pointing readers to issue #1636 for feedback.
- No code changes required during deprecation; the crate and CI job stay in place.

### v0.8.x removal actions (exact files and CI jobs to delete)

1. Delete the crate directory: `crates/windows-gnu-eh/` (Cargo.toml, src/lib.rs).
2. Edit root `Cargo.toml`: remove the `[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]` block (lines 132-133) and the `"crates/windows-gnu-eh"` entry from `[workspace.members]` (line 157).
3. Edit `src/bin/oc-rsync.rs`: drop the `#[cfg(all(target_os = "windows", target_env = "gnu"))] windows_gnu_eh::force_link();` call (lines 16-17).
4. Edit `.github/workflows/ci.yml`: delete the `windows-gnu-cross-check` job (lines 332-359) and any `needs:` reference to it from downstream jobs (none exist today; verify before removal).
5. Update `Cross.toml` if a `[target.x86_64-pc-windows-gnu]` entry is added in the interim (none today).
6. Update `README.md` and `docs/` to remove any mention of GNU support; document MSVC as the sole Windows target.
7. Confirm `release-cross.yml` requires no edits (already MSVC-only).

After removal, the supported Windows target is `x86_64-pc-windows-msvc` exclusively, matching what the project actually ships and what real users consume.
