# Windows GNU vs MSVC: drop, keep, or deprecate

Tracker: #1636. Companion: `docs/audits/windows-gnu-vs-msvc-evaluation.md`
(the long-form predecessor with full citations).

Last verified: 2026-05-07 against master @ `96162aa6e`.

## Summary

Recommendation: **drop the `x86_64-pc-windows-gnu` target** with a one
release window of explicit deprecation in release notes. CI exercises
GNU at the `cargo check` level only, no release artifact targets it,
and no downstream consumer is documented in the tree. Keeping it costs
real review attention every time Windows code changes, with no offsetting
runtime validation.

## 1. Current Windows GNU support

### The shim crate

`crates/windows-gnu-eh/Cargo.toml:1-11` declares a workspace member
that exists for one reason: when cross-compiling to
`x86_64-pc-windows-gnu` with a Zig-bundled MinGW toolchain
(`cargo-zigbuild`), Rust's startup object `rsbegin.o` references
legacy libgcc entry points (`___register_frame_info` /
`___deregister_frame_info`) that the toolchain omits, and the link
fails. The crate provides `#[no_mangle]` shims that forward at
runtime to the modern `__register_frame_info` /
`__deregister_frame_info` symbols in `libgcc_s_seh-1.dll`,
`libgcc_s_sjlj-1.dll`, `libgcc_s_dw2-1.dll`, or `libunwind.dll`,
resolved through `LoadLibraryA` / `GetProcAddress` and cached in
`AtomicUsize` slots
(`crates/windows-gnu-eh/src/lib.rs:77-92`,
`crates/windows-gnu-eh/src/lib.rs:181-197`).

The crate body is 230 lines plus an 11-line `Cargo.toml`. The active
branch is gated on `cfg(all(target_os = "windows", target_env = "gnu"))`
(`crates/windows-gnu-eh/src/lib.rs:61`,
`crates/windows-gnu-eh/src/lib.rs:211`); on every other build, the
public surface collapses to a single `pub const fn force_link() {}`
(`crates/windows-gnu-eh/src/lib.rs:204-209`).

### Wiring into the workspace

Three integration points:

1. Workspace dependency block at `Cargo.toml:131-133`:
   `[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]`
   pulls in `windows-gnu-eh = { path = "crates/windows-gnu-eh" }`.
2. Workspace member registration at `Cargo.toml:157`.
3. The single call site at `src/bin/oc-rsync.rs:15-17`:
   `windows_gnu_eh::force_link()` behind
   `#[cfg(all(target_os = "windows", target_env = "gnu"))]`. The call
   exists only to keep the crate's object file linked into the binary
   so the `#[no_mangle]` shims are visible to `rsbegin.o`.

The toolchain manifest at `rust-toolchain.toml:14-15` also lists both
`x86_64-pc-windows-gnu` and `i686-pc-windows-gnu` in the rustup
`targets` array.

### CI matrix

The relevant jobs in `.github/workflows/` are:

- **`windows-gnu-cross-check`** (`.github/workflows/ci.yml:335-359`):
  runs on `ubuntu-latest`, installs the `x86_64-pc-windows-gnu` Rust
  target plus the system `mingw-w64` package, and executes
  `cargo check --locked --workspace --target x86_64-pc-windows-gnu`
  (`.github/workflows/ci.yml:359`). Timeout 15 minutes
  (`.github/workflows/ci.yml:339`). Type-checking only - no test
  execution, no doctest, no nextest, no runtime validation.
- **`windows-test`** (`.github/workflows/ci.yml:169-216`): runs on
  `windows-latest` with toolchain matrix `[stable, beta, nightly]`,
  builds `--workspace --all-features` and runs
  `cargo nextest run -p core -p engine -p cli --all-features`. MSVC
  only.
- **`windows-iocp`** (`.github/workflows/ci.yml:228-247`): MSVC only,
  exercises the `iocp` feature path explicitly.
- **`windows-acl-xattr`** (`.github/workflows/ci.yml:286-318`): MSVC
  only, exercises `acl,xattr` features.
- **Release packaging** (`.github/workflows/release-cross.yml:597-691`):
  `runs-on: windows-latest`, builds `--target x86_64-pc-windows-msvc`
  only (`.github/workflows/release-cross.yml:616`,
  `.github/workflows/release-cross.yml:640`,
  `.github/workflows/release-cross.yml:670`).

The asymmetry is total: MSVC gets compile + test + IOCP + ACL/xattr +
release artefacts across three toolchains; GNU gets `cargo check` on
one toolchain on a Linux runner.

## 2. Maintenance cost of keeping GNU

**Crate code.** 241 lines (`crates/windows-gnu-eh/`). The crate has had
zero substantive updates since it was added per
`crates/windows-gnu-eh/src/lib.rs:40-46`. The shim references symbols
that have been calling-convention stable since gcc 3.0; no Rust startup
changes have touched the GNU `rsbegin.o` linking model in the last
decade.

**libgcc / libwinpthread bundling.** A Windows GNU binary depends at
runtime on a libgcc DLL plus libwinpthread for thread-local storage,
plus a libstdc++ runtime if any C++ is linked transitively. The shim
already performs lazy `LoadLibraryA` over four candidate library names
(`crates/windows-gnu-eh/src/lib.rs:77-82`); even so, end users either
receive a binary that requires an installed MinGW runtime in `PATH`,
or the build pipeline has to bundle the DLLs alongside `oc-rsync.exe`.
Neither pipeline exists in the tree today: no release workflow targets
GNU, so the bundling question has never been answered. MSVC builds
have no equivalent runtime dependency - they link
`vcruntime140.dll` / `ucrtbase.dll` provided by every supported
Windows version since 10.

**Panic-unwind shim semantics.** The shim's correctness assumption is
that DWARF eh-frame registration is only relevant when libgcc or
libunwind is loaded. If neither is, the shim is a silent no-op
(`crates/windows-gnu-eh/src/lib.rs:115-124`,
`crates/windows-gnu-eh/src/lib.rs:181-187`). That is safe by
construction (no DWARF data is registered, so unwinding has nothing
to do), but it relies on no other crate ever attempting Itanium-style
DWARF unwind on the GNU triple. Rust uses SEH-based unwinding on
`x86_64-pc-windows-gnu` since the panic-strategy switch in 2018, so
the eh-frame path is dormant in practice. A future move (for example,
a third-party crate that statically links libunwind for cross-language
unwind) would force a real implementation rather than the silent
no-op.

**Implicit cross-OS surface tax.** Every Windows-touching change
(roughly, anything in `crates/fast_io/src/iocp/`,
`crates/metadata/src/acl_windows.rs`,
`crates/metadata/src/xattr_windows.rs`,
`crates/transfer/src/disk_commit/writer.rs`) must mentally check
`target_env = "gnu"` as well as `target_env = "msvc"`. In practice
nobody does, because the
`windows-gnu-cross-check` job catches only type errors, not runtime
behaviour. Any regression that compiles cleanly under MinGW but
fails at runtime under GNU goes unnoticed until a downstream user
files it - and there are no downstream users on record.

**CI minutes.** The cross-check job is one of the cheapest in
`.github/workflows/ci.yml`: `ubuntu-latest`, `cargo check`,
15-minute timeout, fast cache. The marginal CI minute spend is
trivial. The maintenance cost is in review and in the false
implication that GNU is supported.

## 3. MSVC-only benefits

**Single ABI to validate.** Today the matrix is `{stable, beta,
nightly} x {MSVC compile-and-test, GNU compile}`, with the GNU
column never running tests. MSVC-only collapses to one ABI with
real test execution everywhere - no hidden second ABI that the
test suite never visits.

**No GNU-EH wiring.** Removing the dependency block at
`Cargo.toml:131-133` and the call site at
`src/bin/oc-rsync.rs:15-17` deletes the only place the binary's
entry point cares about target environment. The `force_link()`
indirection (which exists purely to defeat dead-code stripping for
the shim object) goes away with it. Net subtraction.

**Smaller artifact surface.** No GNU release tarball means no
libgcc/libwinpthread bundling decision, no second SHA256 to publish,
no second toolchain to track in the cross-compile matrix at
`Cargo.toml:368-376` (which already lists only `windows-x86_64 =
true`, so the GNU triple was already marked out-of-scope for
packaging). Documentation rows at `docs/platform-support.md:142`
and the parity matrix at
`docs/audits/cross-platform-parity-matrix.md:238` collapse to a
single Windows row.

**No paper-only `i686-pc-windows-gnu`.** That entry exists at
`rust-toolchain.toml:15` with no CI job, no packaging, and is
explicitly excluded from `Cargo.toml:368-376`'s
`cross_compile_matrix` (`windows-x86 = false`). Dropping GNU
removes the inconsistency.

## 4. User impact

The argument for the GNU target rests on three classes of user;
none is observed in the tree.

**Cross-compile-from-Linux users.** The dominant historical driver
was `cargo-zigbuild`, which used a MinGW-flavoured toolchain by
default. As of 2026, recent `cargo-zigbuild` releases support both
`x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu` triples; the
MSVC path is the recommended one for new projects. Users on this
workflow change one flag (`--target x86_64-pc-windows-msvc`).

The alternative is `cargo-xwin` (Wine-based MSVC cross from
Linux), which is now the standard "cross to Windows from Linux"
recommendation in the Rust community. Neither alternative requires
this project to ship a GNU build.

**MSYS2 / MSYS users.** A user inside an MSYS2 shell with a
GNU-flavoured rustup toolchain is the on-Windows analogue. The
official MSYS2 wiki's current Rust guidance is to install the
`mingw-w64-x86_64-rust` package or to use the rustup MSVC
toolchain inside MSYS2; both are documented paths and neither
requires this project to publish GNU artefacts. There is no
evidence in the tree of a downstream MSYS2 package of `oc-rsync`.

**Distribution packagers.** Some Linux distributions historically
shipped Windows-targeted Rust binaries through MinGW. There is no
record in `docs/`, in issue history, or in the
`docs/audits/cross-platform-parity-matrix.md` audit of any
downstream packager building `oc-rsync` for Windows GNU.

The shared property: none of these workflows have been exercised
end-to-end against this repository. The only artefact CI produces
for GNU is a `cargo check` exit code.

## 5. Recommendation: deprecate one release, then drop

**Drop**, with one minor-version deprecation window.

Decision criteria, applied to the current tree:

| Criterion | Verdict | Evidence |
|----------|---------|----------|
| Does GNU carry user-visible value MSVC does not? | No | Every release artefact and every CI test is MSVC. |
| Does CI exercise GNU meaningfully? | No | `.github/workflows/ci.yml:335-359` is `cargo check` only. |
| Are there documented downstream GNU users? | No | `docs/platform-support.md:142` mentions the crate, no downstream link. |
| Has the IOCP fast path landed? | Yes | `crates/transfer/src/disk_commit/writer.rs:147-150`, dispatched from `crates/fast_io/src/lib.rs:124-128` (#1868). Keeping GNU without runtime validation now means GNU silently misses the fast path on large transfers. |
| Is removal reversible? | Yes | The shim is 230 lines; recovering from git history is one revert. |
| Is bringing GNU to true parity worth it? | No | Parity needs a `windows-latest` GNU runner job (about 45 minutes per push, three toolchains), packaging changes in `release-cross.yml`, and ACL/xattr coverage. That is significant ongoing spend in service of zero observed users. |

### Sunset window

Two release minor versions:

1. **vN.N+1**: mark GNU as deprecated in the release notes for the
   first version after this audit lands. Add a one-paragraph note
   to `docs/platform-support.md:142` indicating removal in vN.N+2.
   The `windows-gnu-cross-check` CI job stays; the toolchain
   manifest entries stay; nothing breaks for existing users.
2. **vN.N+2**: remove. Subtractive deletions only:
   - `crates/windows-gnu-eh/` directory.
   - `Cargo.toml:131-133` (target-conditional dependency block).
   - `Cargo.toml:157` (workspace member entry).
   - `src/bin/oc-rsync.rs:15-17` (`force_link()` call and `cfg`).
   - `.github/workflows/ci.yml:335-359` (`windows-gnu-cross-check`).
   - `rust-toolchain.toml:14-15`
     (`x86_64-pc-windows-gnu`, `i686-pc-windows-gnu`).
   - `docs/platform-support.md:142` (single row).
   - `docs/audits/cross-platform-parity-matrix.md:238`
     (single row).

   Estimated diff: about `-300` lines, no production behaviour
   change (only deletion).

### Communication

Three audiences, one sentence each in the deprecation release note:

- Cross-compile users: switch
  `--target x86_64-pc-windows-gnu` to
  `--target x86_64-pc-windows-msvc` with `cargo-xwin` or
  `cargo-zigbuild` (both support the MSVC triple).
- MSYS2 users: install the `rustup` MSVC toolchain inside the MSYS2
  shell (documented in the MSYS2 Rust wiki).
- Distribution packagers: no observed packagers, but the note exists
  for completeness.

### Reversibility

If a downstream packager appears during the deprecation window, the
deletion is held back and the question is reopened. Until that
happens, the cost-benefit is unambiguous: pay roughly one CI job's
worth of attention and a 230-line crate's worth of conceptual drag,
in service of compile-checking against a target nobody runs.

## 6. References

Code:

- `crates/windows-gnu-eh/Cargo.toml:1-11`
- `crates/windows-gnu-eh/src/lib.rs:1-230`
- `Cargo.toml:131-133` (target-conditional dependency)
- `Cargo.toml:157` (workspace member)
- `Cargo.toml:368-376` (`cross_compile_matrix`)
- `src/bin/oc-rsync.rs:15-17` (`force_link()` call site)
- `rust-toolchain.toml:10-18` (rustup target list)

CI:

- `.github/workflows/ci.yml:169-216` (`windows-test`, MSVC, three
  toolchains)
- `.github/workflows/ci.yml:228-247` (`windows-iocp`, MSVC)
- `.github/workflows/ci.yml:286-318` (`windows-acl-xattr`, MSVC)
- `.github/workflows/ci.yml:335-359` (`windows-gnu-cross-check`,
  GNU, `cargo check` only)
- `.github/workflows/release-cross.yml:597-691` (Windows MSVC
  release packaging; `release-cross.yml:616`,
  `release-cross.yml:640`, `release-cross.yml:670` pin the MSVC
  triple)

Adjacent docs:

- `docs/audits/windows-gnu-vs-msvc-evaluation.md` (long-form
  predecessor)
- `docs/platform-support.md:142`
- `docs/audits/cross-platform-parity-matrix.md:238`
