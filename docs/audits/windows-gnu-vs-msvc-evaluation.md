# Windows GNU vs MSVC: drop-or-keep evaluation

Tracker: #1636. Predecessors: #1633 (usage audit), #1634 (maintenance scope
inventory), #1635 / #1742 (CI cross-check job for `x86_64-pc-windows-gnu`).
Adjacent: #1868 (IOCP wired into the disk-commit pipeline), #1869 (Windows
ACL/xattr CI matrix), #1389 (ReFS reflink), #1900 (IOCP-only CI job),
#1928 (IOCP socket I/O).

Last verified: 2026-05-05. No code changes in this audit.

## 1. Why this question matters now

For the project's first year on Windows, both `x86_64-pc-windows-msvc`
and `x86_64-pc-windows-gnu` were treated as more or less symmetric: MSVC
shipped the release artifact, GNU was kept compilable so cross-compilers
on Linux (notably `cargo-zigbuild`) had a path. That symmetry has now
ended on the Windows side.

The change of context comes from #1868. Until that issue, the IOCP code
in `crates/fast_io/src/iocp/` existed but no transfer code reached for
it - the disk-commit thread always picked the buffered writer on
Windows. PR #3698 wired `fast_io::IocpDiskBatch` into the disk-commit
state machine so that, on Windows builds with the `iocp` feature, large
file writes go through overlapped I/O via `Writer::Iocp`
(`crates/transfer/src/disk_commit/writer.rs:147-150`,
`writer.rs:182-184`, `writer.rs:236-237`). The `iocp` cargo feature is
default-on at the workspace root (`Cargo.toml:24-35`,
`Cargo.toml:77`) and at the `fast_io` crate (`fast_io/Cargo.toml:39`,
`fast_io/Cargo.toml:55`), so any user building with `--release`
defaults gets the IOCP fast path on Windows.

The IOCP module itself is large: `4608` lines across eleven files in
`crates/fast_io/src/iocp/` (`file_factory.rs:665`, `pump.rs:781`,
`disk_batch.rs:988`, `socket.rs:628`, `file_writer.rs:454`,
`file_reader.rs:409`, `config.rs:204`, `error.rs:165`,
`overlapped.rs:152`, `completion_port.rs:109`, `mod.rs:53`). All of it
compiles only when both `target_os = "windows"` and the `iocp` feature
are set (`crates/fast_io/src/lib.rs:124-128`). On any non-Windows or
non-MSVC build that does not set the feature, the stub module
`iocp_stub.rs` is compiled in place and degrades to standard buffered
writes. The `transfer` Writer enum follows the same gating
(`crates/transfer/src/disk_commit/writer.rs:147`).

The practical question becomes: Windows MSVC now has a measurably
different code path with its own large body of code. Does the Windows
GNU target carry that path? Does it carry the ACL and xattr paths in
`metadata`? If not, what exactly does keeping GNU buy versus what does
it cost?

## 2. Current state

### What `windows-gnu-eh` actually does

The crate is a single 230-line library
(`crates/windows-gnu-eh/src/lib.rs`) with an 11-line `Cargo.toml`
(`crates/windows-gnu-eh/Cargo.toml`). It provides two `#[no_mangle]`
shim symbols, `___register_frame_info` and `___deregister_frame_info`
(`lib.rs:181-187`, `lib.rs:191-197`), plus a `force_link()` no-op
(`lib.rs:199-202`). The shims forward at runtime to the modern
`__register_frame_info`/`__deregister_frame_info` symbols in either
`libgcc_s_seh-1.dll`, `libgcc_s_sjlj-1.dll`, `libgcc_s_dw2-1.dll`, or
`libunwind.dll` (`lib.rs:77-82`), resolved through `LoadLibraryA` /
`GetProcAddress` (`lib.rs:87-92`). If none of the candidate libraries
provide the symbols, the shim is a silent no-op
(`lib.rs:115`, `lib.rs:181-187`), which is safe because DWARF unwind
metadata exists only when unwinding is active.

The crate's reason for existing is documented inline at `lib.rs:8-19`:
when cross-compiling with `cargo-zigbuild` for `x86_64-pc-windows-gnu`,
the Zig-bundled MinGW toolchain omits the legacy libgcc entry points
that Rust's `rsbegin.o` startup object references, causing link
failures. The shim restores the legacy names and forwards them to the
modern symbols at runtime.

The crate is dependency-free beyond `kernel32` and ships only `core`
imports (`lib.rs:62-66`). The non-Windows-GNU branch
(`lib.rs:204-209`) is a single `pub const fn force_link() {}` no-op.

The single integration point in the rest of the workspace is the
`force_link()` call at `src/bin/oc-rsync.rs:16-17` behind
`#[cfg(all(target_os = "windows", target_env = "gnu"))]`. The
dependency itself is gated identically at `Cargo.toml:131-133`. On
all other configurations the crate is not even reached by the linker.

### What CI does for Windows GNU

`ci.yml:216-240` defines `windows-gnu-cross-check`. It runs on
`ubuntu-latest`, installs the `x86_64-pc-windows-gnu` Rust target and
the system `mingw-w64` package, and runs
`cargo check --locked --workspace --target x86_64-pc-windows-gnu`
(`ci.yml:240`). The job has a 15-minute timeout (`ci.yml:220`). It
performs no test execution: nothing under `crates/*/tests/` is run for
the GNU triple, no integration test, no doctest, no nextest run.

The MSVC side is fundamentally different. `ci.yml:165-211` (job
`windows-test`) runs on `windows-latest` with toolchain matrix
`stable`, `beta`, `nightly`, builds `--workspace --all-features`
(`ci.yml:194-195`), and runs `cargo nextest run -p core -p engine
-p cli --all-features` (`ci.yml:197-201`). Release artifact builds in
`release-cross.yml:594-680` target only `x86_64-pc-windows-msvc`
(`release-cross.yml:616`, `release-cross.yml:640`,
`release-cross.yml:670`). No release workflow targets
`x86_64-pc-windows-gnu`.

Asymmetry: MSVC gets compile + test + release packaging across three
toolchains; GNU gets compile-only on a single toolchain.

### Toolchain configuration

`rust-toolchain.toml:10-18` lists seven cross-compile targets. Of those,
four are Windows: `x86_64-pc-windows-gnu`, `i686-pc-windows-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`. The `i686` GNU
entry has no CI job and is not declared in
`Cargo.toml:368-376`'s `cross_compile_matrix`, which lists only
`windows-x86_64 = true` and `windows-x86 = false`. So three of the four
listed Windows targets are paper toolchain entries with no CI evidence.

### IOCP gating recap

`crates/fast_io/src/lib.rs:124-128`: IOCP module is selected by
`#[cfg(all(target_os = "windows", feature = "iocp"))]`. The
`target_env` is not part of the gate. In principle, an
`x86_64-pc-windows-gnu` build with `--features iocp` would compile the
real IOCP module and link against `windows-sys 0.61`
(`crates/fast_io/Cargo.toml:69-74`). In practice the `windows-gnu-cross-check`
job runs `cargo check`, not `cargo build`, and runs without
`--features iocp` selection beyond the workspace defaults, so the IOCP
linkage on GNU is type-checked but never exercised against the actual
MinGW import libraries.

The Windows ACL crate (`crates/metadata/Cargo.toml:38-45`) and the
Windows xattr code (`crates/metadata/src/xattr_windows.rs`,
`crates/metadata/src/acl_windows.rs`) follow `cfg(windows)` with no
`target_env` distinction, so on paper GNU compiles them too. There
is no test evidence for any of this on the GNU triple.

## 3. User cases for Windows GNU

The argument for keeping the GNU target rests on three classes of user.

**Cross-compilation from Linux without a Windows host.** The dominant
real-world driver is `cargo-zigbuild`, which ships a MinGW-flavoured
toolchain and uses `target_env = "gnu"`. CI uses this approach
indirectly via the `mingw-w64` package on `ubuntu-latest`
(`ci.yml:231`). Engineers who want a Windows binary from a Linux laptop
or a Linux container generally pick GNU because it requires no
licensing for the Windows SDK and headers. This is the primary group
the existing `windows-gnu-eh` crate was added for, per the rationale at
`crates/windows-gnu-eh/src/lib.rs:8-19`.

**Distribution-style packagers.** Some Linux distributions package
Windows-targeted Rust binaries through MinGW. There is no evidence in
the tree that any downstream packager is currently doing this for
oc-rsync; the docs at `docs/platform-support.md:138-142` only flag the
existence of the GNU exception-handling crate, not a downstream
consumer.

**MSYS2/MSYS-style users on Windows.** A user inside an MSYS2 shell
running a GNU-flavoured Rust toolchain is the on-Windows version of the
same case: their preinstalled C library is `libgcc`, not the MSVC CRT.
There is no first-party MSYS2 package of oc-rsync on the tree, but
nothing structurally prevents it.

The shared property is that **none of these workflows have been
exercised end-to-end** in this repository: there is no workflow that
builds a runnable GNU binary, runs its test suite, or packages it.
`ci.yml:216-240` confirms `cargo check` only.

## 4. User cases for Windows MSVC only

**Native Windows builds.** Every Windows release artifact is MSVC
(`release-cross.yml:616`, `release-cross.yml:640`,
`release-cross.yml:670`). Every Windows test run in CI is MSVC
(`ci.yml:165-211`). Every binary downloaded from the project's release
page is MSVC.

**IOCP fast path.** `iocp` is in the workspace's default features set
(`Cargo.toml:33`). It pulls in `transfer/iocp` and `fast_io/iocp`
(`Cargo.toml:77`). The transfer crate's disk-commit thread dispatches
to `Writer::Iocp { batch }` (`crates/transfer/src/disk_commit/writer.rs:147-150`)
exactly when `target_os = "windows"` and the feature is set. The IOCP
implementation links `windows-sys 0.61` directly
(`crates/fast_io/Cargo.toml:69-74`). The `windows-sys` crate provides
import libraries that work on both MSVC and GNU, so on GNU the
compilation is technically possible. What is not technically possible
without further work is end-to-end validation of the IOCP path under
GNU - the existing CI `cargo check` step does not exercise it.

**ACL surface.** `crates/metadata/src/acl_windows.rs` is an 814-line
file gated `#![cfg(all(feature = "acl", windows))]` (`acl_windows.rs:1`).
It links the `windows` crate (`crates/metadata/Cargo.toml:38-45`) for
`Win32_Security_Authorization` to call `GetSecurityInfo` /
`SetSecurityInfo`. The `windows` crate from microsoft/windows-rs
supports both MSVC and GNU triples, but again this combination is not
exercised in CI - the `windows-acl-xattr` job that #1869 envisioned is
not on master, and the existing `windows-gnu-cross-check` runs without
`--features acl,xattr`.

**xattr surface.** `crates/metadata/src/xattr_windows.rs` is 560 lines
implementing NTFS Alternate Data Streams. Same gating as ACL.

**Mature ecosystem.** The dominant Rust crates that touch the Win32 API
(notably `windows`, `windows-sys`, `windows-targets`) ship their import
libraries in MSVC and GNU flavours, but the maintained, tested combination
is MSVC. The official Rust toolchain advertises MSVC as Tier 1 for
`x86_64-pc-windows-msvc` and Tier 1 for `x86_64-pc-windows-gnu`, but the
documented set of preinstalled tooling on GitHub-hosted runners is the
MSVC ecosystem; the GNU side requires an explicit `mingw-w64`
installation step (`ci.yml:231-232`).

## 5. Maintenance cost of keeping Windows GNU

**Code.** `windows-gnu-eh` is 230 lines plus 11 lines of `Cargo.toml`
(241 lines total). The active branch is gated to
`cfg(all(target_os = "windows", target_env = "gnu"))`
(`lib.rs:61`, `lib.rs:211`), so on every other build it is a single
`pub const fn force_link() {}` (`lib.rs:204-209`). Maintenance of the
shim itself has been zero since it was added, per the inline comment
at `lib.rs:40-46`.

**CI minutes.** The `windows-gnu-cross-check` job (`ci.yml:216-240`)
has a 15-minute timeout (`ci.yml:220`). It runs once per push on
`ubuntu-latest`, which is the cheapest GitHub-hosted runner. Steady
state with cache hits the actual elapsed time is well under the
timeout. Compared to the other CI jobs in the same file, the GNU
cross-check is one of the cheapest entries, since it is `cargo check`
only on a Linux runner.

**eh-frame fixup complexity.** The interesting maintenance question is
not the line count but the semantic stability of the shim. The shim
relies on Rust's startup object `rsbegin.o` continuing to call
`___register_frame_info` and on libgcc/libunwind continuing to expose
`__register_frame_info`. Both of those calling conventions have been
stable since at least gcc 3.0 and have not moved through any of the
LLVM/libgcc transitions of the past decade. The crate's own header at
`lib.rs:40-46` notes "no updates are required unless Rust changes its
startup object linking model for the GNU target," which has not
occurred and is not on any roadmap visible to this audit.

**Implicit cross-OS surface.** The wider cost is not the shim itself
but the implication that any Windows code change must be considered
against `target_env = "gnu"` as well as `target_env = "msvc"`. In
practice nobody does this, because the `cargo check` job catches only
type errors. A regression that compiles but fails at runtime under GNU
will not be caught by current CI. The visible signature of this is
that #1633 (usage audit) had to write the audit precisely because
the question was unanswered.

## 6. Risk of dropping GNU

**Loss of the cross-compile-from-Linux story.** The most concrete
loss is users running `cargo-zigbuild` on Linux to produce a Windows
binary. This is a real workflow but, on the evidence of #1633's
follow-on tasks, has no documented downstream user in this project.
Removing GNU forces those users to either install Windows in a VM or
container, or use a Wine-based MSVC cross toolchain
(`xwin` or `cargo-xwin`). The `xwin` workflow is well-established in
the Rust community and is what most "cross to Windows" instructions
recommend in 2026.

**Loss of the MSYS2 path.** A user inside an MSYS2 GNU shell loses
the ability to build oc-rsync from source with their default
toolchain. They must switch to an MSVC-flavoured toolchain in the
same shell. There is no evidence on the tree that anyone is doing
this build today.

**Features that become unreachable.** Nothing in the current
codebase is reachable only on GNU. Every feature gate is structured
around `target_os = "windows"` plus an optional `feature = "iocp"`
or `feature = "acl"`, never around `target_env`. Removing GNU
removes a set of compile permutations but does not remove any
user-visible behaviour.

**Visible breakage.** The `windows-gnu-cross-check` job
(`ci.yml:216-240`) goes away. The
`x86_64-pc-windows-gnu` and `i686-pc-windows-gnu` lines in
`rust-toolchain.toml:14-15` go away. The `[target.'cfg(all(windows,
target_env = "gnu"))'.dependencies]` block at
`Cargo.toml:131-133` goes away. The `crates/windows-gnu-eh/`
directory goes away. The `force_link()` call at
`src/bin/oc-rsync.rs:16-17` and the `mod` registration go away.
The `crates/windows-gnu-eh` line at `Cargo.toml:157` goes away. The
parity-matrix row at `docs/audits/cross-platform-parity-matrix.md:238`
and the platform-support row at `docs/platform-support.md:142` need to
be removed.

## 7. Migration path if dropping

The removal is purely subtractive. There is no symbol that has to be
preserved under another name; the `windows-gnu-eh` shims are
externally referenced only by Rust's own startup object on the GNU
target. On MSVC the shims are not present and not referenced.

**Delete:**

1. `crates/windows-gnu-eh/` directory in full.
2. The workspace member entry at `Cargo.toml:157`.
3. The conditional dependency block at `Cargo.toml:131-133`.
4. The `force_link()` call and `cfg` import at
   `src/bin/oc-rsync.rs:16-17`.
5. The `windows-gnu-cross-check` job in `.github/workflows/ci.yml`
   (`ci.yml:213-240`, including the comment banner).
6. The `x86_64-pc-windows-gnu` and `i686-pc-windows-gnu` entries from
   `rust-toolchain.toml:14-15`.
7. Documentation rows at `docs/platform-support.md:142` and
   `docs/audits/cross-platform-parity-matrix.md:238`.

**Deprecate (one release window):**

The most defensible cadence is to mark the GNU target as deprecated in
release notes for one minor version, then remove it in the next.
Because none of the deprecation period requires code changes, the
deprecation step can simply be a one-paragraph entry in the next
`docs/CHANGELOG.md` block plus a notice in `docs/platform-support.md`
that the existing row at `:142` is scheduled for removal. The GNU
cross-check job stays during deprecation; only after the deprecation
window closes do the deletions in the previous list happen.

**Communicate:**

Three audiences:

1. Cross-compile users: tell them to use `cargo-xwin` or
   `cargo-zigbuild --target x86_64-pc-windows-msvc` (zigbuild supports
   both flavours). The replacement is one flag change.
2. MSYS2 users: point them at the rustup MSVC toolchain inside MSYS2,
   which is a documented path.
3. Distro packagers: there is no evidence of any, so this is a
   precautionary mention in release notes only.

The release-notes wording can be a single sentence: "Builds against
the `x86_64-pc-windows-gnu` target are no longer supported. Use
`x86_64-pc-windows-msvc`. The `windows-gnu-eh` compatibility crate
has been removed."

Estimated diff: roughly `-300` lines workspace-wide, with no production
code changes (only deletion).

## 8. Migration path if keeping

If the decision is to keep GNU as a first-class target rather than a
"compiles, possibly works" target, the work to bring it to parity is
significantly larger than the saved 300 lines if dropped.

**IOCP availability validation.** The IOCP module already gates only on
`target_os = "windows"` and `feature = "iocp"`
(`crates/fast_io/src/lib.rs:124-128`), so it should compile under GNU.
What is missing is an end-to-end test that the runtime behaviour is
correct: that `windows-sys 0.61` (`crates/fast_io/Cargo.toml:69-74`)
imports resolve under MinGW's import libraries, that
`FILE_SKIP_SET_EVENT_ON_HANDLE` is honoured, that the completion port
pump (`crates/fast_io/src/iocp/pump.rs`) drains correctly. This needs a
new CI job: `windows-gnu-iocp-test` that builds with `--features iocp`
and runs `cargo nextest run -p fast_io -p transfer` on a Windows
runner with a GNU toolchain. That is a `windows-latest` job, not the
existing `ubuntu-latest` cross-check. Cost: roughly the same CI minutes
as the existing `windows-test` job (45-minute timeout, three toolchain
matrix entries if we mirror MSVC).

**ACL/xattr validation.** The `metadata` crate's Windows surface
(`acl_windows.rs:814` lines, `xattr_windows.rs:560` lines) compiles
under GNU but is not tested there. Parity requires either extending
the existing `windows-test` matrix (`ci.yml:165-211`) to GNU, or
adding a sibling job. `cargo nextest run -p metadata --features
acl,xattr` is already tracked under #1869 as a gap on the MSVC side;
covering both triples doubles that work.

**Release artifacts.** If GNU is supported, it should ship a release
binary. That requires a sibling job to `release-cross.yml:594-680`
that runs the same xtask packaging step
(`release-cross.yml:635-640`) with `--tarball-target
x86_64-pc-windows-gnu`. The `xtask` packager and the renaming step
(`release-cross.yml:642-660`) both need to learn the GNU triple.

**i686 GNU.** `rust-toolchain.toml:15` lists `i686-pc-windows-gnu` but
no CI job. Either delete that line or add a job; status quo is
ambiguous.

**Total cost to bring GNU to parity:** at minimum one additional
Windows runner per push (about 45 minutes of `windows-latest` time,
times three toolchains if matrix is preserved), plus packaging, plus
ongoing review burden every time a Windows-specific PR lands.

## 9. Recommendation

**Drop the Windows GNU target.**

Decision criteria, applied to the current tree:

1. Is the target carrying user-visible value that no other target
   provides? *No.* MSVC builds reach every feature, every test, and
   every release artifact (`release-cross.yml:594-680`,
   `ci.yml:165-211`).
2. Does CI exercise the target meaningfully? *No.* The
   `windows-gnu-cross-check` job
   (`ci.yml:216-240`) runs `cargo check` only - no tests, no run-time
   validation.
3. Is there evidence of downstream users on the GNU triple? *No.*
   `docs/platform-support.md:142` mentions the crate; no audit or
   issue links a downstream package.
4. Has the IOCP fast path landed on Windows? *Yes.* PR #3698
   (#1868) wired `IocpDiskBatch` into the disk-commit thread
   (`crates/transfer/src/disk_commit/writer.rs:147-150`). Keeping
   GNU without parity validation now means the GNU build silently
   has worse runtime behaviour than the MSVC build for any large
   transfer.
5. Is the cost of dropping reversible if a downstream user appears?
   *Yes.* The 230-line `windows-gnu-eh` crate is small enough to
   restore from git history, and the `cargo-xwin` / `cargo-zigbuild`
   ecosystems have caught up with the cross-from-Linux story.
6. Is the cost of bringing GNU to true parity worth the spend?
   *No.* Per Section 8, parity requires a Windows runner job
   (about 45 minutes of `windows-latest` per push times three
   toolchains), plus release packaging, plus ACL/xattr coverage.
   That is a significant ongoing cost in service of a target with
   zero observed downstream consumers.

If criterion 1 or 3 changes (for example, a downstream packager
appears, or upstream `windows-sys` drops MSVC support), revisit.

## 10. Open questions

1. **Does any downstream packager build oc-rsync for Windows GNU
   today?** If yes, the deprecation window in Section 7 should be at
   least one full release cycle, not a single minor version.
2. **What is the cargo-zigbuild story going forward?** Recent
   versions of `cargo-zigbuild` claim to support both MSVC and GNU
   triples for Windows; if the MSVC support is mature enough that
   `cargo-zigbuild` users can transparently switch the target flag,
   that strengthens the recommendation to drop GNU.
3. **Does anyone build inside MSYS2 with a GNU rustup toolchain?**
   The MSYS2 wiki documents both options; current Rust guidance is
   to use the MSVC toolchain even from inside MSYS2.
4. **What does the `i686-pc-windows-gnu` target in
   `rust-toolchain.toml:15` accomplish today?** It has no CI job, is
   excluded from `Cargo.toml:368-376`'s `cross_compile_matrix`
   (`windows-x86 = false` at line 376), and is not in
   `[workspace.metadata.oc_rsync.cross_compile]` (line 366 lists
   `windows = ["x86_64"]` only). It looks like dead configuration
   regardless of the GNU/MSVC decision.
5. **If the recommendation is accepted, who scrubs the rustup
   targets list and the `cross_compile_matrix`?** That is a follow-up
   task, not part of the 1636 deliverable.
6. **Should the deletion be staged?** Section 7 sketches one minor
   version of deprecation. An alternative is to delete in a single
   commit and rely on the release notes alone. The choice depends on
   the answer to question 1.
7. **Does the `windows-gnu-cross-check` job catch any class of
   regression that the `windows-test` job does not?** Empirically,
   none observed since the job was added (#1635 / #1742). The job
   functions as a "compiles under MinGW" smoke test; if no
   downstream user needs that property, the job's signal value is
   close to zero.

## 11. References

Code:

- `crates/windows-gnu-eh/Cargo.toml:1-11`
- `crates/windows-gnu-eh/src/lib.rs:1-230`
- `src/bin/oc-rsync.rs:15-22`
- `Cargo.toml:24-35` (default features)
- `Cargo.toml:77` (`iocp` feature wiring)
- `Cargo.toml:131-133` (GNU dep block)
- `Cargo.toml:157` (workspace member)
- `Cargo.toml:368-376` (cross-compile matrix)
- `crates/fast_io/Cargo.toml:39` (`iocp` default)
- `crates/fast_io/Cargo.toml:55` (`iocp` feature definition)
- `crates/fast_io/Cargo.toml:69-74` (`windows-sys` import surface)
- `crates/fast_io/src/lib.rs:124-128` (IOCP gating)
- `crates/transfer/src/disk_commit/writer.rs:147-150` (Iocp Writer
  variant)
- `crates/transfer/src/disk_commit/writer.rs:182-184` (write
  dispatch)
- `crates/transfer/src/disk_commit/writer.rs:236-237` (commit
  dispatch)
- `crates/metadata/Cargo.toml:38-45` (Windows ACL deps)
- `crates/metadata/src/acl_windows.rs:1` (file gate)
- `rust-toolchain.toml:10-18` (target list)

CI:

- `.github/workflows/ci.yml:165-211` (windows-test, MSVC)
- `.github/workflows/ci.yml:213-240` (windows-gnu-cross-check)
- `.github/workflows/release-cross.yml:594-680` (Windows MSVC
  release packaging)
- `.github/workflows/release-cross.yml:616, 640, 670` (MSVC triple
  references)

Adjacent docs:

- `docs/platform-support.md:142`
- `docs/audits/cross-platform-parity-matrix.md:238`
- `docs/audits/windows-acl-xattr-ci-matrix.md:1-50` (related #1869
  context on Windows test surface)
- `docs/design/iocp-transfer-pipeline-wiring.md:1` (#1868 design)
